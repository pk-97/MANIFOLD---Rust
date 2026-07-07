//! `--script <file.json>` runner (`UI_AUTOMATION_DESIGN.md` §6, P2 §9). A
//! JSON array of `AutomationAction`s (`manifold_ui::automation`) executed in
//! order against a scene fixture. Artifacts land in
//! `target/ui-snapshots/<scene>/run-<script-stem>/`: a numbered PNG/dump per
//! `Snapshot`/`Dump` step, plus `result.json`. Exit 0 only if every step
//! succeeded (D6, D10).
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
//! empty, host-independent, never the user's file. (Pre-2026-07-07 this
//! driver mirrored a single `LayerClicked` arm and logged everything else
//! unapplied, which made every transport/inspector action invisible to
//! headless verification — the seam the dead-LANES investigation exposed.)

use std::path::PathBuf;

use manifold_core::LayerId;
use manifold_editing::command::Command;
use manifold_ui::automation::{
    self, AssertCheck, AutomationAction, AutomationTarget, Gesture, MatchInfo,
};
use manifold_ui::clip_hit_tester::ClipHitTargets;
use manifold_ui::hit_targets::HitTargets;
use manifold_ui::input::{PointerAction, UIEvent};
use manifold_ui::interaction_overlay::InteractionOverlay;
use manifold_ui::node::{Rect, Vec2};
use manifold_ui::panels::PanelAction;
use manifold_ui::automation_hit_tester::AutomationHitTargets;

use super::fixtures::SceneData;
use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::editing_host::AppEditingHost;
use crate::ui_root::{ScrollDirty, UIRoot};

/// Fixed per-`Step` delta — the fixture's project runs at 60 fps default
/// (`CLAUDE.md`); the driver's clock only advances on `Step` (D7), never a
/// wall-clock read.
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
             scrollshrink, hairlineclips, automation, selectionclips)"
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
    if scene == "inspector" {
        ui.layout.inspector_width = 600.0;
        ui.layout.timeline_split_ratio = 0.6;
    } else {
        ui.layout.inspector_width = 0.0;
        ui.layout.timeline_split_ratio = 0.93;
    }
    super::sync_build(&mut ui, &data, zoom_ppb);

    let mut runner = Runner::new();
    let mut results = Vec::with_capacity(actions.len());
    let mut ok = true;

    for (index, action) in actions.iter().enumerate() {
        let outcome = runner.step(&mut ui, &mut data, zoom_ppb, index, action, &out_dir);
        let failed = outcome.status == "fail";
        results.push(outcome);
        if failed {
            ok = false;
            break;
        }
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
    needs_rebuild: bool,
    needs_structural_sync: bool,
    scroll_dirty: ScrollDirty,
    invalidate_layers: Vec<usize>,
    pre_drag_commands: Vec<Box<dyn Command>>,
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
    audio_crossover_snapshot: Option<(f32, f32)>,
    audio_send_gain_drag_snapshot: Option<f32>,
    audio_send_sensitivity_drag_snapshot: Option<Vec<manifold_core::audio_trigger::TriggerRoute>>,
    active_inspector_drag: Option<crate::app::ActiveInspectorDrag>,
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
            needs_rebuild: false,
            needs_structural_sync: false,
            scroll_dirty: ScrollDirty::default(),
            invalidate_layers: Vec::new(),
            pre_drag_commands: Vec::new(),
            clock: 0.0,
            modifiers: manifold_ui::input::Modifiers::NONE,
            user_prefs: crate::user_prefs::UserPrefs::in_memory(),
            slider_snapshot: None,
            trim_snapshot: None,
            target_snapshot: None,
            decay_snapshot: None,
            audio_shape_snapshot: None,
            audio_crossover_snapshot: None,
            audio_send_gain_drag_snapshot: None,
            audio_send_sensitivity_drag_snapshot: None,
            active_inspector_drag: None,
        }
    }

    fn step(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        zoom_ppb: f32,
        index: usize,
        action: &AutomationAction,
        out_dir: &std::path::Path,
    ) -> StepResult {
        let action_desc = format!("{action:?}");
        match action {
            AutomationAction::Step { frames } => {
                self.clock += *frames as f32 * DT;
                StepResult {
                    index,
                    action: action_desc,
                    status: "ok",
                    detail: format!("clock -> {:.3}s", self.clock),
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
                self.write_png(ui, data, &path);
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
                ui.key_event(*key, *modifiers);
                // Was `let _ = ui.process_events()` — key-triggered panel
                // actions were resolved and then dropped on the floor (the
                // same dormant seam as the pre-2026-07-07 apply_panel_actions).
                self.drain_and_dispatch(ui, data);
                self.rebuild(ui, data, zoom_ppb);
                StepResult {
                    index,
                    action: action_desc,
                    status: "ok",
                    detail: format!("key {key:?}"),
                    artifact: None,
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
                self.pointer(ui, data, zoom_ppb, index, action_desc, target, gesture, out_dir)
            }
            AutomationAction::Assert { selector, check } => {
                self.assert(ui, data, index, action_desc, selector, check, out_dir)
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
    ) -> StepResult {
        let rect = match self.resolve(ui, data, target) {
            Ok(r) => r,
            Err(e) => return self.fail(index, action_desc, ui, data, out_dir, e),
        };
        let center = Vec2::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);

        match gesture {
            Gesture::Click { modifiers } => {
                self.modifiers = *modifiers;
                ui.input.set_modifiers(*modifiers);
                ui.pointer_event(center, PointerAction::Down, self.clock);
                ui.pointer_event(center, PointerAction::Up, self.clock);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::DoubleClick => {
                ui.input.set_modifiers(self.modifiers);
                ui.pointer_event(center, PointerAction::Down, self.clock);
                ui.pointer_event(center, PointerAction::Up, self.clock);
                ui.pointer_event(center, PointerAction::Down, self.clock);
                ui.pointer_event(center, PointerAction::Up, self.clock);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::Hover => {
                ui.pointer_event(center, PointerAction::Move, self.clock);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::Scroll { delta } => {
                ui.input.process_scroll(center, *delta);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::Drag { to, steps } => {
                let to_rect = match self.resolve(ui, data, to) {
                    Ok(r) => r,
                    Err(e) => return self.fail(index, action_desc, ui, data, out_dir, e),
                };
                let to_center = Vec2::new(to_rect.x + to_rect.width * 0.5, to_rect.y + to_rect.height * 0.5);
                ui.input.set_modifiers(self.modifiers);
                ui.pointer_event(center, PointerAction::Down, self.clock);
                for pt in automation::interpolate_drag(center, to_center, *steps) {
                    ui.pointer_event(pt, PointerAction::Move, self.clock);
                }
                ui.pointer_event(to_center, PointerAction::Up, self.clock);
                self.drain_and_dispatch(ui, data);
            }
        }

        self.rebuild(ui, data, zoom_ppb);
        StepResult {
            index,
            action: action_desc,
            status: "ok",
            detail: format!("acted at ({:.1},{:.1})", center.x, center.y),
            artifact: None,
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
        let mut host = AppEditingHost::new(
            &mut data.project,
            &self.content_tx,
            &self.content_state,
            &mut self.cursor_manager,
            &mut self.active_layer,
            &mut self.needs_rebuild,
            &mut self.needs_structural_sync,
            &mut self.scroll_dirty,
            &mut self.invalidate_layers,
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
        let _ = host.pending_actions; // context-menu actions: no proving script needs these yet
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
                &mut self.audio_crossover_snapshot,
                &mut self.audio_send_gain_drag_snapshot,
                &mut self.audio_send_sensitivity_drag_snapshot,
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

    fn rebuild(&mut self, ui: &mut UIRoot, data: &SceneData, zoom_ppb: f32) {
        super::sync_build(ui, data, zoom_ppb);
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

    fn write_png(&self, ui: &UIRoot, data: &SceneData, path: &std::path::Path) {
        let tex_w = (super::LOGICAL_W * super::SCALE) as u32;
        let tex_h = (super::LOGICAL_H * super::SCALE) as u32;
        super::render::render_ui_to_png(
            ui,
            &data.selection,
            &data.content.automation_latched_params,
            tex_w,
            tex_h,
            super::SCALE,
            false,
            path.to_str().expect("utf-8 path"),
        );
    }
}

/// One-shot click dispatch for `interact.rs`'s `select:` sugar (§6 — the
/// existing `select:`/`open:` verbs become one-step scripts compiled to the
/// §4 core). Resolves a layer header by its display `text` and fires a real
/// `Click` through the exact same core the `--script` runner uses
/// (`Runner::resolve` + `drain_and_dispatch`) — no bespoke reimplementation.
/// `Err` on a miss (D6): the caller surfaces it loudly instead of guessing at
/// a fallback index; no fallback path exists here.
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
