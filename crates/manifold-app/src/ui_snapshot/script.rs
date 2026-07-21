//! `--script <file.json>` runner (`UI_AUTOMATION_DESIGN.md` §6, P2 §9). A
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
//! `Runner` no longer reimplements the
//! App's invalidate/rebuild decision and no longer renders through
//! `render_ui_to_png`'s full-repaint lookalike. It now owns a persistent
//! [`RenderState`] (one `UICacheManager` + composited offscreen for the
//! whole script run) and drives every frame through
//! `crate::ui_frame::apply_ui_frame_invalidations` +
//! `crate::ui_frame::composite_main_ui_frame` — the SAME two functions the
//! live App and the P0 differential (`ui_snapshot::mod::cache_path_full_render`)
//! call. One update+composite path, three callers.
//!
//! `AppEditingHost` (`editing_host.rs`) writes two signals this seam's
//! `UiFrameSignals` has no field for (see [`Runner::drain_and_dispatch`]):
//! a completed structural drag sets the seam's OWN one-shot rebuild flag
//! (the field `UiFrameSignals` clears after consuming, not
//! `needs_structural_sync`) — but that flag and `needs_structural_sync` OR
//! into the identical full-rebuild branch of `apply_ui_frame_invalidations`,
//! so this module folds the drag signal into `needs_structural_sync` instead
//! (exactly equivalent, never named by its own identifier here); per-layer
//! bitmap invalidation is a live-app-only Pass-4c mechanism this headless
//! harness never renders, so it's a dead sink, same as it always was.

use std::path::PathBuf;
use std::time::Duration;

use manifold_core::LayerId;
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::undo::UndoRedoManager;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::ui_cache_manager::UICacheManager;
use manifold_renderer::ui_renderer::UIRenderer;
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
use crate::ui_frame::{apply_ui_frame_invalidations, UiFrameSignals};
use crate::ui_root::{ScrollDirty, UIRoot};

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

/// Run `script_path`'s `AutomationAction` array against `scene`. Exits the
/// process: 0 if every step succeeded, 1 otherwise (D6/D10 — no partial
/// pass). `LOGICAL_W`/`LOGICAL_H`/`SCALE`/`zoom_ppb` mirror
/// `render_ui_scene`'s own fixed values so a script's rects agree with the
/// plain `--dump`/`--interact` runs of the same scene.
pub fn run(scene: &str, script_path: &str) {
    let Some(mut data) = super::fixtures::build(scene) else {
        eprintln!(
            "ui-snap --script: unknown scene '{scene}' (known: timeline, states, inspector, \
             paramsteps, scrollshrink, hairlineclips, automation, selectionclips, gltfscene, \
             gltfanimscene, envmod)"
        );
        std::process::exit(2);
    };
    let script_text = match std::fs::read_to_string(script_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ui-snap --script: can't read '{script_path}': {e}");
            std::process::exit(2);
        }
    };
    let actions: Vec<AutomationAction> = match serde_json::from_str(&script_text) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ui-snap --script: '{script_path}' doesn't parse: {e}");
            std::process::exit(2);
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
        || scene == "bug060"
        || scene == "bug060heavy"
        || scene == "paramsteps"
        || scene == "gltfscene"
        || scene == "gltfanimscene"
        || scene == "bug047"
        || scene == "envmod"
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
    if scene == "audiosends" {
        ui.audio_setup_panel.open();
        ui.layout.audio_setup_width = manifold_ui::color::DEFAULT_AUDIO_SETUP_WIDTH;
        ui.audio_setup_panel.set_scope_bands(250.0, 2000.0, 10.0, 22_000.0);
    }
    super::sync_build(&mut ui, &data, zoom_ppb);

    // P2 (D3): ONE persistent cache + composited offscreen for the whole
    // script run, seeded with a full self-clearing composite — mirrors
    // `cache_path_full_render`'s own "Frame 0". Every later step updates
    // this SAME state through `Runner`'s seam calls; a `Snapshot` only ever
    // reads it back (see `Runner::write_png`).
    let tex_w = (super::LOGICAL_W * super::SCALE) as u32;
    let tex_h = (super::LOGICAL_H * super::SCALE) as u32;
    let mut render = RenderState::new(tex_w, tex_h);
    composite_frame(
        &render.device,
        &mut render.ui_renderer,
        &mut render.cache,
        &mut ui,
        &render.composite,
        1.0,
    );

    let mut runner = Runner::new();
    // BUG-296: `resolve_effect_target` falls back to `timeline.layers.first()`
    // when `Runner.active_layer` is None, silently mutating the wrong layer's
    // effect chain for structural PanelActions dispatched before any
    // `LayerClicked` gesture. Seed it from the fixture's actual active layer
    // (`data.active`) so a script's first structural dispatch (e.g.
    // `EffectReorder`) targets the layer the display is actually showing.
    runner.active_layer = data
        .active
        .and_then(|i| data.project.timeline.layers.get(i).map(|l| l.layer_id.clone()));
    let mut results = Vec::with_capacity(actions.len());
    let mut ok = true;

    for (index, action) in actions.iter().enumerate() {
        let outcome = runner.step(&mut ui, &mut data, zoom_ppb, index, action, &out_dir, &mut render);
        let failed = outcome.status == "fail";
        results.push(outcome);
        if failed {
            ok = false;
            break;
        }
    }

    // D9a: a contact sheet of every frame a `Step` action advanced. Most
    // flows carry no `Step` (the two shipped flows don't), so most runs
    // write nothing here — this is additive, not a new requirement on every
    // script.
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

    if !ok {
        eprintln!("ui-snap --script: FAILED — see {}", result_path.display());
        std::process::exit(1);
    }
}

/// Persistent GPU render state the [`Runner`] drives through the shared seam
/// (P2, D3) — ONE `UICacheManager` + composited offscreen for the whole
/// script run, updated after every dirtying action exactly as the live App's
/// own cache is, not rebuilt from scratch per `Snapshot` (the drift P2
/// exists to kill). Kept OUT of `Runner` itself (rather than a field on it)
/// because `Runner::new()` is also used standalone by [`click_by_text`],
/// which never renders a pixel — building a `GpuDevice`/atlas there would be
/// pure waste. `pub fn run` constructs this once and threads it through
/// every `Runner::step` call.
struct RenderState {
    device: GpuDevice,
    ui_renderer: UIRenderer,
    cache: UICacheManager,
    composite: CompositeResources,
    tex_w: u32,
    tex_h: u32,
}

impl RenderState {
    /// D8: scale factor 1.0 always, at the fixture's logical size (matches
    /// every other headless caller of the seam).
    fn new(tex_w: u32, tex_h: u32) -> Self {
        let device = GpuDevice::new();
        let ui_renderer = UIRenderer::new(&device, GpuTextureFormat::Bgra8Unorm);
        let mut cache = UICacheManager::new(GpuTextureFormat::Bgra8Unorm, 1.0);
        cache.set_scale_factor(1.0);
        cache.ensure_atlas(&device, tex_w, tex_h);
        cache.invalidate_all();
        let composite = CompositeResources::new(&device, tex_w, tex_h);
        Self { device, ui_renderer, cache, composite, tex_w, tex_h }
    }

    /// Read back the CURRENTLY composited offscreen — raw BGRA8 bytes (the
    /// seam's own format). Does not re-composite; callers drive that
    /// separately (`composite_frame`, called from `Runner::advance_frame` /
    /// the `Step` loop).
    fn readback_bgra(&self) -> Vec<u8> {
        super::render::readback(&self.device, &self.composite.offscreen, self.tex_w, self.tex_h)
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
    // P2: the Runner's persistent halves of `UiFrameSignals` (D3) — folded
    // into a fresh `UiFrameSignals` at each seam call (`advance_frame` /
    // the `Step` loop) and written back afterward. No standalone one-shot
    // rebuild field here (see the module doc): a completed structural drag
    // folds into `needs_structural_sync` instead, which the seam treats
    // identically.
    needs_structural_sync: bool,
    scroll_dirty: ScrollDirty,
    pre_drag_commands: Vec<Box<dyn Command>>,
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
    slider_snapshot: Option<f32>,
    trim_snapshot: Option<(f32, f32)>,
    target_snapshot: Option<f32>,
    decay_snapshot: Option<f32>,
    audio_shape_snapshot: Option<manifold_core::audio_mod::AudioModShape>,
    audio_action_snapshot: Option<manifold_core::audio_mod::TriggerAction>,
    audio_crossover_snapshot: Option<(f32, f32)>,
    audio_send_gain_drag_snapshot: Option<f32>,
    active_inspector_drag: Option<crate::app::ActiveInspectorDrag>,
    // D9a: every composited frame a `Step` action advanced, in order —
    // assembled into one contact-sheet PNG at the end of `run` when
    // non-empty. D9b: the most recent `Pointer` gesture's synthesized
    // point(s) (center, interpolated drag path, drag end) — consumed
    // (drained) by the next `Snapshot`, which stamps a crosshair at each
    // one on the readback COPY only.
    filmstrip: Vec<Vec<u8>>,
    last_gesture_points: Vec<Vec2>,
    // BUG-234: persistent scratch for `evaluate_modulation`'s per-tick
    // clip-timing/trigger-pulse buffers — reused across `Step` frames so the
    // harness's modulation tick allocates nothing extra per frame, matching
    // the content thread's own `PlaybackEngine` fields
    // (`modulation_timing_scratch`/`pending_trigger_pulses`).
    modulation_timing_scratch: Vec<(manifold_core::Beats, manifold_core::Beats)>,
    modulation_pulses: Vec<manifold_playback::modulation::TriggerPulse>,
}

impl Runner {
    fn new() -> Self {
        let (content_tx, _content_rx) = crossbeam_channel::bounded(64);
        Self {
            overlay: InteractionOverlay::new(manifold_ui::color::CLIP_VERTICAL_PAD),
            content_tx,
            _content_rx,
            content_state: ContentState::default(),
            cursor_manager: manifold_ui::cursors::CursorManager::default(),
            active_layer: None,
            needs_structural_sync: false,
            scroll_dirty: ScrollDirty::default(),
            pre_drag_commands: Vec::new(),
            undo: UndoRedoManager::new(),
            clock: 0.0,
            modifiers: manifold_ui::input::Modifiers::NONE,
            user_prefs: crate::user_prefs::UserPrefs::in_memory(),
            slider_snapshot: None,
            trim_snapshot: None,
            target_snapshot: None,
            decay_snapshot: None,
            audio_shape_snapshot: None,
            audio_action_snapshot: None,
            audio_crossover_snapshot: None,
            audio_send_gain_drag_snapshot: None,
            active_inspector_drag: None,
            filmstrip: Vec::new(),
            last_gesture_points: Vec::new(),
            modulation_timing_scratch: Vec::new(),
            modulation_pulses: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn step(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        zoom_ppb: f32,
        index: usize,
        action: &AutomationAction,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> StepResult {
        let action_desc = format!("{action:?}");
        let outcome = match action {
            AutomationAction::Step { frames } => {
                // P2 (D3, D9a): each stepped frame drives the REAL seam —
                // `ui.update()` (advances any wall-clock-driven tween, e.g.
                // the inspector drawer) → the rebuild/cache-invalidate
                // decision → composite — then captures a filmstrip tile.
                // Mirrors `cache_path_full_render`'s own drawer-tween loop
                // (P0), generalized to any script's `Step` action.
                for _ in 0..*frames {
                    self.clock += DT;
                    std::thread::sleep(Duration::from_secs_f32(DT));
                    ui.update();
                    if ui.inspector.drawer_anim_active() {
                        self.needs_structural_sync = true;
                    }
                    // BUG-234: the harness's frame-advance never ran the
                    // modulation pipeline, so no flow could show a
                    // driver/envelope-modulated value change (VD-031). Wire
                    // the SAME public tick entry point the content thread's
                    // `PlaybackEngine::tick_playing`/`tick_non_playing` call
                    // (`engine.rs:911`/`:1020`) directly against the
                    // fixture's `data.project` — no new wrapper, no
                    // manifold-playback change. Audio is empty (headless has
                    // no capture backend); clip-edge layers empty (no
                    // sync_clips_to_time driver here to populate it — the
                    // envelope path this proves runs off elapsed-since-start,
                    // not the edge queue).
                    let current_beat = manifold_core::tempo::TempoMapConverter::seconds_to_beat(
                        &mut data.project.tempo_map,
                        manifold_core::Seconds(self.clock as f64),
                        data.project.settings.bpm,
                    );
                    let mut fire_meters = manifold_core::audio_trigger::FireMeterCapture::default();
                    let modulation_dirty = manifold_playback::modulation::evaluate_modulation(
                        &mut data.project,
                        current_beat,
                        manifold_core::Seconds(DT as f64),
                        &manifold_core::audio_features::AudioFeatureSnapshot::default(),
                        &mut self.modulation_timing_scratch,
                        &mut self.modulation_pulses,
                        &[],
                        &mut fire_meters,
                    );
                    if modulation_dirty {
                        self.needs_structural_sync = true;
                    }
                    let mut signals = UiFrameSignals {
                        needs_structural_sync: self.needs_structural_sync,
                        scroll_dirty: self.scroll_dirty,
                        ..Default::default()
                    };
                    apply_ui_frame_invalidations(ui, Some(&mut render.cache), &mut signals);
                    self.needs_structural_sync = signals.needs_structural_sync;
                    self.scroll_dirty = signals.scroll_dirty;
                    // BUG-234 cont'd: `apply_ui_frame_invalidations` only
                    // decides whether to rebuild STRUCTURE; the already-built
                    // widgets' VALUE text (the slider fill/label
                    // `push_state`/`UiParamSlot::value` writes) is only
                    // refreshed by `reconcile_state`, same as every other
                    // per-frame caller (`advance_frame` above calls both, in
                    // this same order). Without this, `evaluate_modulation`'s
                    // write into `data.project`'s param `effective` value
                    // never reaches the widget tree a `Dump`/`Snapshot` reads.
                    super::reconcile_state(ui, data);
                    composite_frame(
                        &render.device,
                        &mut render.ui_renderer,
                        &mut render.cache,
                        ui,
                        &render.composite,
                        1.0,
                    );
                    self.filmstrip.push(render.readback_bgra());
                }
                StepResult {
                    index,
                    action: action_desc,
                    status: "ok",
                    detail: format!("clock -> {:.3}s ({frames} filmstrip tile(s))", self.clock),
                    artifact: None,
                }
            }
            AutomationAction::Dump => {
                let path = out_dir.join(format!("{index:02}.tree.json"));
                self.write_dump(ui, data, &path);
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
                    let undone = self.undo.undo(&mut data.project);
                    // Undo can add/remove nodes, layers, clips — always
                    // force a structural resync rather than guessing per
                    // command (same conservative call `commit_command_batch`
                    // makes for a completed drag).
                    self.needs_structural_sync = true;
                    self.advance_frame(ui, data, zoom_ppb, render, false);
                    StepResult {
                        index,
                        action: action_desc,
                        status: "ok",
                        detail: format!("undo -> {undone} (undo_count={})", self.undo.undo_count()),
                        artifact: None,
                    }
                } else if modifiers.is_command_shift() && *key == Key::Z {
                    let redone = self.undo.redo(&mut data.project);
                    self.needs_structural_sync = true;
                    self.advance_frame(ui, data, zoom_ppb, render, false);
                    StepResult {
                        index,
                        action: action_desc,
                        status: "ok",
                        detail: format!("redo -> {redone} (redo_count={})", self.undo.redo_count()),
                        artifact: None,
                    }
                } else if !modifiers.is_none() {
                    self.fail(
                        index,
                        action_desc,
                        ui,
                        data,
                        out_dir,
                        format!(
                            "no headless seam for modifier-bearing Key {key:?} ({modifiers:?}) — \
                             only Cmd+Z (undo) / Cmd+Shift+Z (redo) are wired; see BUG-198"
                        ),
                    )
                } else {
                    ui.key_event(*key, *modifiers);
                    // Was `let _ = ui.process_events()` — key-triggered panel
                    // actions were resolved and then dropped on the floor (the
                    // same dormant seam as the pre-2026-07-07 apply_panel_actions).
                    self.drain_and_dispatch(ui, data);
                    self.advance_frame(ui, data, zoom_ppb, render, false);
                    StepResult {
                        index,
                        action: action_desc,
                        status: "ok",
                        detail: format!("key {key:?}"),
                        artifact: None,
                    }
                }
            }
            AutomationAction::Text { .. } => {
                // No headless injection seam exists yet: text editing lives
                // entirely in `Application::text_input` (manifold-app), which
                // `UIRoot` can't reach (no `pub fn text_event` on `UIRoot` —
                // re-derived while building this driver). Fails loudly (D6)
                // rather than silently no-op-ing; §7's live door is the
                // precedent for wiring this once P3 has a live Application.
                self.fail(index, action_desc, ui, data, out_dir, "no headless seam for AutomationAction::Text (Application::text_input only; see UI_AUTOMATION_DESIGN.md §7)".into())
            }
            AutomationAction::Pointer { target, gesture } => {
                self.pointer(ui, data, zoom_ppb, index, action_desc, target, gesture, out_dir, render)
            }
            AutomationAction::Assert { selector, check } => {
                self.assert(ui, data, index, action_desc, selector, check, out_dir)
            }
            AutomationAction::ScrollTo { target } => {
                self.scroll_to(ui, data, zoom_ppb, index, action_desc, target, out_dir, render)
            }
        };
        // BUG-198: whatever this step just sent over `content_tx` (via
        // `AppEditingHost`/`ui_bridge::dispatch`) has already been applied
        // directly to `data.project` — record it into the harness's own
        // undo stack so `Key`'s Cmd+Z/Cmd+Shift+Z arm has something real to
        // act on. Every step drains unconditionally (most steps sent
        // nothing, so this is a no-op `try_recv` miss).
        self.record_executed_commands();
        outcome
    }

    /// Drains every `ContentCommand` this step's `AppEditingHost`/
    /// `ui_bridge::dispatch` calls sent over `content_tx` (the receiver is
    /// otherwise held only to keep the channel alive — see `Runner::new`)
    /// and folds the editing ones into the harness's `undo` stack. NEVER
    /// re-executes: `AppEditingHost`/`ui_bridge` always mutate
    /// `data.project` synchronously before sending (mirroring the live
    /// app's `local_project` pattern, module doc), so by the time this runs
    /// the command has already taken effect — `record()`, not `execute()`.
    /// Non-editing variants (`SeekTo`, `Undo`, …) are inert here; the
    /// harness's headless `Key` arm drives `self.undo` directly instead of
    /// routing through this channel.
    fn record_executed_commands(&mut self) {
        while let Ok(cmd) = self._content_rx.try_recv() {
            match cmd {
                ContentCommand::Execute(cmd) => self.undo.record(cmd),
                ContentCommand::ExecuteBatch(cmds, desc) => {
                    self.undo.record(Box::new(CompositeCommand::new(cmds, desc)));
                }
                _ => {}
            }
        }
    }

    fn surfaces_owned(&self, ui: &UIRoot, data: &SceneData) -> (Vec<manifold_ui::panels::viewport::ClipScreenRect>, Vec<manifold_ui::panels::viewport::AutomationLaneScreen>) {
        let mut clip_rects = Vec::new();
        ui.viewport.visible_clip_rects(&mut clip_rects);
        let automation_lanes = ui.viewport.automation_lane_screens(&data.content.automation_latched_params);
        (clip_rects, automation_lanes)
    }

    fn resolve(
        &self,
        ui: &UIRoot,
        data: &SceneData,
        target: &AutomationTarget,
    ) -> Result<Rect, String> {
        let (clip_rects, automation_lanes) = self.surfaces_owned(ui, data);
        let clip_targets = ClipHitTargets(&clip_rects);
        let automation_targets = AutomationHitTargets(&automation_lanes);
        let surfaces: Vec<&dyn HitTargets> = vec![&clip_targets, &automation_targets];
        automation::resolve(&ui.tree, &surfaces, target)
            .map(|r| r.rect)
            .map_err(|e| e.to_string())
    }

    fn resolve_all(
        &self,
        ui: &UIRoot,
        data: &SceneData,
        target: &AutomationTarget,
    ) -> (Vec<MatchInfo>, String) {
        let (clip_rects, automation_lanes) = self.surfaces_owned(ui, data);
        let clip_targets = ClipHitTargets(&clip_rects);
        let automation_targets = AutomationHitTargets(&automation_lanes);
        let surfaces: Vec<&dyn HitTargets> = vec![&clip_targets, &automation_targets];
        automation::resolve_all(&ui.tree, &surfaces, target)
    }

    #[allow(clippy::too_many_arguments)]
    fn pointer(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        zoom_ppb: f32,
        index: usize,
        action_desc: String,
        target: &AutomationTarget,
        gesture: &Gesture,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> StepResult {
        let rect = match self.resolve(ui, data, target) {
            Ok(r) => r,
            Err(e) => return self.fail(index, action_desc, ui, data, out_dir, e),
        };
        let center = Vec2::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);

        // D9b: this gesture's point(s) replace whatever the last one left —
        // consumed (drained) by the next Snapshot only.
        self.last_gesture_points.clear();
        let mut scrolled_in_place = false;

        match gesture {
            Gesture::Click { modifiers } => {
                self.modifiers = *modifiers;
                ui.input.set_modifiers(*modifiers);
                self.last_gesture_points.push(center);
                ui.pointer_event(center, PointerAction::Down, self.clock);
                ui.pointer_event(center, PointerAction::Up, self.clock);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::DoubleClick => {
                ui.input.set_modifiers(self.modifiers);
                self.last_gesture_points.push(center);
                ui.pointer_event(center, PointerAction::Down, self.clock);
                ui.pointer_event(center, PointerAction::Up, self.clock);
                ui.pointer_event(center, PointerAction::Down, self.clock);
                ui.pointer_event(center, PointerAction::Up, self.clock);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::RightClick => {
                self.last_gesture_points.push(center);
                ui.input.process_right_click(&ui.tree, center);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::Hover => {
                self.last_gesture_points.push(center);
                ui.pointer_event(center, PointerAction::Move, self.clock);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::Scroll { delta } => {
                scrolled_in_place = self.scroll_once(ui, data, center, *delta);
            }
            Gesture::Drag { to, steps } => {
                let to_rect = match self.resolve(ui, data, to) {
                    Ok(r) => r,
                    Err(e) => return self.fail(index, action_desc, ui, data, out_dir, e),
                };
                let to_center = Vec2::new(to_rect.x + to_rect.width * 0.5, to_rect.y + to_rect.height * 0.5);
                ui.input.set_modifiers(self.modifiers);
                self.last_gesture_points.push(center);
                ui.pointer_event(center, PointerAction::Down, self.clock);
                for pt in automation::interpolate_drag(center, to_center, *steps) {
                    self.last_gesture_points.push(pt);
                    ui.pointer_event(pt, PointerAction::Move, self.clock);
                }
                self.last_gesture_points.push(to_center);
                ui.pointer_event(to_center, PointerAction::Up, self.clock);
                self.drain_and_dispatch(ui, data);
            }
        }

        self.advance_frame(ui, data, zoom_ppb, render, scrolled_in_place);
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
    /// set `scrolled_in_place`); the Scene dock sets its offset and forces the
    /// rebuild that bakes it in (BUG-294); anything else routes the generic
    /// pipeline. Returns whether the inspector handled the scroll in place.
    fn scroll_once(&mut self, ui: &mut UIRoot, data: &mut SceneData, center: Vec2, delta: Vec2) -> bool {
        if ui.layout.inspector().contains(center) {
            if ui.try_inspector_scroll(delta.y, center.x) {
                return ui.inspector.take_scrolled_in_place();
            }
            // Nothing built yet — the real handler's same fallback.
            ui.inspector.handle_scroll_at(delta.y, center.x);
        } else if ui.scene_setup_panel.is_open() && ui.layout.scene_setup().contains(center) {
            // BUG-294: direct offset setter + forced structural sync, because the
            // generic `UIEvent::Scroll` pipeline alone never rebuilds the Scene
            // dock's built node positions (`window_input.rs` sets `needs_rebuild`
            // right after routing scene-dock scroll; this mirrors it).
            ui.scene_setup_panel.handle_scroll(delta.y);
            self.needs_structural_sync = true;
        } else {
            ui.input.process_scroll(center, delta);
            self.drain_and_dispatch(ui, data);
        }
        false
    }

    /// Scroll `target`'s enclosing container until it sits inside the visible
    /// band, so a following `Pointer` step can act on a row that laid out far
    /// below the fold (`WIDGET_TREE_DESIGN.md` §5/§6 converged card rows —
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
        zoom_ppb: f32,
        index: usize,
        action_desc: String,
        target: &AutomationTarget,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> StepResult {
        // A chunky wheel notch: the Scene dock scales the raw delta by its own
        // SCROLL_SPEED (~0.375 → ~900px/step), so the deepest rows reach well
        // inside the cap. Success is judged on the target's CENTER (that is where
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
        let rect0 = match self.resolve(ui, data, target) {
            Ok(r) => r,
            Err(e) => return self.fail(index, action_desc, ui, data, out_dir, e),
        };
        let cx = rect0.x + rect0.width * 0.5;
        let insp = ui.layout.inspector();
        let scene = ui.layout.scene_setup();
        let in_x = |r: Rect| r.width > 0.0 && cx >= r.x && cx < r.x + r.width;
        let container_is_inspector = if in_x(insp) {
            true
        } else if ui.scene_setup_panel.is_open() && in_x(scene) {
            false
        } else {
            return self.fail(
                index,
                action_desc,
                ui,
                data,
                out_dir,
                format!("ScrollTo: target column x={cx:.0} is in no open scroll container (inspector {insp:?}, scene {scene:?})"),
            );
        };
        let container_rect = |ui: &UIRoot| {
            if container_is_inspector {
                ui.layout.inspector()
            } else {
                ui.layout.scene_setup()
            }
        };

        // +1: a negative delta reveals lower content (target's Y decreases toward
        // the band). Both containers share this convention today; detecting it on
        // the first productive step keeps ScrollTo correct if one ever flips.
        let mut sign = 1.0_f32;
        let mut sign_locked = false;

        for _ in 0..MAX_ITERS {
            let rect = match self.resolve(ui, data, target) {
                Ok(r) => r,
                Err(e) => return self.fail(index, action_desc.clone(), ui, data, out_dir, e),
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
            let sip = self.scroll_once(ui, data, sc, Vec2::new(0.0, dir * sign * STEP));
            self.advance_frame(ui, data, zoom_ppb, render, sip);
            let moved = match self.resolve(ui, data, target) {
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
        }

        match self.resolve(ui, data, target) {
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
                        data,
                        out_dir,
                        format!(
                            "ScrollTo: exhausted {MAX_ITERS} steps, target center y={cy_target:.0} still outside band [{:.0},{:.0}]",
                            vis.y + MARGIN,
                            vis.y + vis.height - MARGIN
                        ),
                    )
                }
            }
            Err(e) => self.fail(index, action_desc, ui, data, out_dir, e),
        }
    }

    /// Drain events queued by the pointer/key synthesis above through the
    /// SAME per-frame call the live app makes (`app_render.rs:869`), then
    /// route any tracks-area events (clip click/drag) through the overlay —
    /// the exact real path `app_render.rs`'s viewport-events block uses,
    /// minus the live `Application` (see the module doc for why that's
    /// still the real path, not a parallel one).
    fn drain_and_dispatch(&mut self, ui: &mut UIRoot, data: &mut SceneData) {
        let actions = ui.process_events();
        self.apply_panel_actions(ui, data, &actions);

        let viewport_events = ui.drain_viewport_events();
        if viewport_events.is_empty() {
            return;
        }
        self.overlay.set_modifiers(self.modifiers);
        // Local, call-scoped sinks for the two `AppEditingHost` outputs the
        // P2 seam doesn't consume by name (see the module doc): a completed
        // structural drag folds into `needs_structural_sync` below (the seam
        // ORs the two identically); per-layer bitmap invalidation is a
        // live-app-only Pass-4c mechanism this harness never renders, so
        // it's a dead sink here exactly as it always was.
        let mut rebuild_flag = false;
        let mut layer_bitmap_scratch: Vec<usize> = Vec::new();
        let mut host = AppEditingHost::new(
            &mut data.project,
            &self.content_tx,
            &self.content_state,
            &mut self.cursor_manager,
            &mut self.active_layer,
            &mut rebuild_flag,
            &mut self.needs_structural_sync,
            &mut self.scroll_dirty,
            &mut layer_bitmap_scratch,
            &mut self.pre_drag_commands,
        );
        for event in &viewport_events {
            match event {
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
                UIEvent::DragBegin { origin, .. } => {
                    self.overlay.on_begin_drag(*origin, &mut host, &mut data.selection, &ui.viewport);
                }
                UIEvent::Drag { pos, .. } => {
                    self.overlay.on_drag(*pos, &mut host, &mut data.selection, &mut ui.viewport);
                }
                UIEvent::DragEnd { .. } => {
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
        let pending = std::mem::take(&mut host.pending_actions);
        drop(host);
        if rebuild_flag {
            self.needs_structural_sync = true;
        }
        self.apply_panel_actions(ui, data, &pending);
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
            let result = crate::ui_bridge::dispatch(
                action,
                &mut data.project,
                &self.content_tx,
                &self.content_state,
                ui,
                &mut data.selection,
                &mut self.active_layer,
                &mut self.slider_snapshot,
                &mut self.trim_snapshot,
                &mut self.target_snapshot,
                &mut self.decay_snapshot,
                &mut self.audio_shape_snapshot,
                &mut self.audio_action_snapshot,
                &mut self.audio_crossover_snapshot,
                &mut self.audio_send_gain_drag_snapshot,
                &mut self.user_prefs,
                &mut self.active_inspector_drag,
                None,
            );
            println!("ui-snap --script: dispatched {action:?} (structural={})", result.structural_change);
            if result.structural_change {
                self.needs_structural_sync = true;
            }
            // The fixture's active-layer INDEX feeds `sync_build`'s inspector
            // sync; derive it from the id the real bridge maintains (the old
            // mirrored arm set it directly).
            if let PanelAction::LayerClicked(..) = action {
                data.active = self
                    .active_layer
                    .as_ref()
                    .and_then(|lid| data.project.timeline.find_layer_by_id(lid).map(|(i, _)| i));
            }
        }
    }

    /// Drives one frame through the shared seam (P2, D3): re-sync `ui` from
    /// `data` (the Runner's own responsibility headless — no
    /// continuously-running content thread to do it every tick), gate the
    /// rebuild/cache-invalidation decision through
    /// `apply_ui_frame_invalidations` using the REAL structural/scroll
    /// signals `drain_and_dispatch`/`AppEditingHost` already collected,
    /// reconcile display state, THEN composite — matching `sync_build`'s own
    /// order (`ui.build()` before `push_state`/`ui.update()`) and the live
    /// tick's order (the invalidation decision before `present_all_windows`,
    /// which is the actual atlas paint).
    fn advance_frame(
        &mut self,
        ui: &mut UIRoot,
        data: &SceneData,
        zoom_ppb: f32,
        render: &mut RenderState,
        scrolled_in_place: bool,
    ) {
        super::sync_data(ui, data, zoom_ppb);
        // BUG-073 fix shape (b): this driver has no per-frame timer, so a
        // tween a dispatch just armed (e.g. a newly-armed drawer growing a
        // card's row count) would otherwise sit at its t=0 state forever —
        // `Snapshot`/`Dump` don't rebuild, they just read whatever THIS call
        // last produced, so settling has to happen here, before the rebuild
        // decision below. Only forces a rebuild when something was actually
        // mid-flight, so a script with nothing armed keeps the same
        // cache-hit behavior it had before this fix.
        let settled = ui.inspector.skip_to_settled(&mut ui.tree);
        let mut signals = UiFrameSignals {
            needs_structural_sync: self.needs_structural_sync || settled,
            scroll_dirty: self.scroll_dirty,
            scrolled_in_place,
            ..Default::default()
        };
        apply_ui_frame_invalidations(ui, Some(&mut render.cache), &mut signals);
        self.needs_structural_sync = signals.needs_structural_sync;
        self.scroll_dirty = signals.scroll_dirty;
        super::reconcile_state(ui, data);
        composite_frame(
            &render.device,
            &mut render.ui_renderer,
            &mut render.cache,
            ui,
            &render.composite,
            1.0,
        );
    }

    fn assert(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        index: usize,
        action_desc: String,
        selector: &AutomationTarget,
        check: &AssertCheck,
        out_dir: &std::path::Path,
    ) -> StepResult {
        let (matches, query) = self.resolve_all(ui, data, selector);
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
        };

        match result {
            Ok(detail) => StepResult { index, action: action_desc, status: "ok", detail, artifact: None },
            Err(detail) => self.fail(index, action_desc, ui, data, out_dir, detail),
        }
    }

    /// D6: a failure carries the dump as evidence — write it now regardless
    /// of whether the failing step was itself a `Dump`.
    fn fail(
        &mut self,
        index: usize,
        action_desc: String,
        ui: &UIRoot,
        data: &SceneData,
        out_dir: &std::path::Path,
        detail: String,
    ) -> StepResult {
        let path = out_dir.join(format!("{index:02}.fail.tree.json"));
        self.write_dump(ui, data, &path);
        StepResult {
            index,
            action: action_desc,
            status: "fail",
            detail,
            artifact: Some(path.display().to_string()),
        }
    }

    fn write_dump(&self, ui: &UIRoot, data: &SceneData, path: &std::path::Path) {
        let (clip_rects, automation_lanes) = self.surfaces_owned(ui, data);
        let clip_targets = ClipHitTargets(&clip_rects);
        let automation_targets = AutomationHitTargets(&automation_lanes);
        let surfaces: Vec<&dyn HitTargets> = vec![&clip_targets, &automation_targets];
        let json = super::dump::dump_tree_ex(&ui.tree, &surfaces);
        std::fs::write(path, serde_json::to_string_pretty(&json).expect("serialize dump"))
            .expect("write dump json");
    }

    /// P2 (D3): reads back the offscreen `advance_frame`/the `Step` loop
    /// already composited through the shared seam — no fresh device/cache is
    /// built here (that was `render_ui_to_png`'s old, always-full-clear
    /// shape; see `render.rs`'s module doc). Draws the SAME
    /// `crate::ui_frame::render_main_ui_passes` the live app and
    /// `render_ui_to_png` call (P2, `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md`
    /// §4 step 2 — no more parallel `draw_immediate_passes`) on top, then
    /// stamps D9b's pointer crosshair(s) CPU-side on the readback COPY —
    /// never into the atlas/offscreen, which would poison it for any later
    /// frame or the P0 differential shelf tool. No thumbnails here (this
    /// driver never opted into `--thumbs`, matching the old call's `false`).
    fn write_png(&mut self, ui: &mut UIRoot, data: &SceneData, render: &mut RenderState, path: &std::path::Path) {
        let (tex_w, tex_h) = (render.tex_w, render.tex_h);
        let mut clip_rects = Vec::new();
        ui.viewport.visible_clip_rects(&mut clip_rects);
        let hovered_clip = ui.viewport.hovered_clip_id();
        let clip_bodies: Vec<manifold_renderer::clip_draw::ClipBody> = clip_rects
            .iter()
            .map(|cr| manifold_renderer::clip_draw::ClipBody {
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
        let automation_lanes =
            ui.viewport.automation_lane_screens(&data.content.automation_latched_params);
        let text_input = crate::text_input::TextInputState::new();
        let frame_timer = crate::frame_timer::FrameTimer::new(60.0);
        crate::ui_frame::render_main_ui_passes(
            &render.device,
            &mut render.ui_renderer,
            ui,
            &render.composite.offscreen,
            tex_w,
            tex_h,
            f64::from(super::SCALE),
            crate::ui_frame::MainUiPassInputs {
                layer_bitmap_gpu: None,
                clip_bodies: &clip_bodies,
                clip_rects: &clip_rects,
                clip_content_gpu: None,
                thumb: None,
                timeline_overlays: manifold_ui::panels::viewport::TimelineOverlays::default(),
                markers: &[],
                landing_flash: None,
                automation_lanes: &automation_lanes,
                cursor_pos: manifold_ui::node::Vec2::ZERO,
                text_input: &text_input,
                frame_timer: &frame_timer,
                vqt: None,
                blit_pipeline: &render.composite.blit_pipeline,
                blit_sampler: &render.composite.blit_sampler,
                gpu_sink: None,
            },
        );
        let mut bgra = render.readback_bgra();
        for pt in self.last_gesture_points.drain(..) {
            stamp_crosshair(&mut bgra, tex_w, tex_h, pt);
        }
        super::render::save_bgra_png(&bgra, tex_w, tex_h, path);
    }
}

/// D9b: draw a small crosshair (~11px, opaque red) centered at `pt` (logical
/// == texel here — the harness is always scale factor 1.0, D8) directly into
/// BGRA8 bytes of stride `tex_w * 4`. CPU-side only; never called on a
/// texture, only on a readback `Vec<u8>` already destined for a PNG.
fn stamp_crosshair(bgra: &mut [u8], tex_w: u32, tex_h: u32, pt: Vec2) {
    const RADIUS: i32 = 5;
    const COLOR: [u8; 4] = [0, 0, 255, 255]; // BGRA8: opaque red
    let (cx, cy) = (pt.x.round() as i32, pt.y.round() as i32);
    let mut plot = |x: i32, y: i32| {
        if x < 0 || y < 0 || x as u32 >= tex_w || y as u32 >= tex_h {
            return;
        }
        let off = ((y as u32 * tex_w + x as u32) * 4) as usize;
        bgra[off..off + 4].copy_from_slice(&COLOR);
    };
    for d in -RADIUS..=RADIUS {
        plot(cx + d, cy);
        plot(cx, cy + d);
    }
}

/// One-shot click dispatch for `interact.rs`'s `select:` sugar (§6 — the
/// existing `select:`/`open:` verbs become one-step scripts compiled to the
/// §4 core). Resolves a layer header by its display `text` and fires a real
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
    let rect = runner.resolve(ui, data, &target)?;
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
pub(super) fn write_fail_dump(ui: &UIRoot, data: &SceneData, path: &std::path::Path) {
    Runner::new().write_dump(ui, data, path);
}
