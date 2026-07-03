//! Debounced background autosave — GIG_RESILIENCE_DESIGN §6, phase P1.
//!
//! Trigger: dirty-debounced. `AUTOSAVE_DEBOUNCE` after the LAST edit, and only
//! while `EditingService` reports dirty — never a blind wall-clock save. One
//! save per edit burst: after firing, the timer stays disarmed until a new
//! edit (data_version change while dirty) re-arms it.
//!
//! Zero-hitch: serialization happens on a background thread from the UI's
//! retained `Arc<Project>` snapshot (`Application::last_snapshot_arc` — the
//! snapshot channel that already exists). The UI thread only clones an Arc
//! pointer and a handful of scalars. Everything routes through the existing
//! `manifold_io::saver::save_project(…, is_auto = true)`, which gives the
//! `history/` journal entry and auto-save pruning for free (D4). No new save
//! path.
//!
//! Perform mode parks the timer (D5) by construction: `tick_and_render`
//! short-circuits to the perform HUD before `tick_autosave` runs.
//!
//! The worker thread is Result-hardened per D7 (no unwrap/expect on fallible
//! paths — under `panic = "abort"` a worker panic kills the show).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use manifold_core::project::Project;

use crate::app::Application;

/// Seconds of edit silence before a dirty project autosaves (§6 default).
pub(crate) const AUTOSAVE_DEBOUNCE: Duration = Duration::from_secs(60);

/// Debounce + in-flight bookkeeping for the background autosave.
/// Pure state machine — time is injected so tests don't sleep.
pub(crate) struct AutosaveState {
    /// `data_version` of the most recent edit observed while dirty.
    last_edit_version: u64,
    /// When that edit was observed. `None` = disarmed.
    last_edit_at: Option<Instant>,
    /// The version the last fired autosave covered — prevents re-firing
    /// on wall-clock alone.
    saved_version: u64,
    /// Completion channel of the in-flight background save, if any.
    in_flight: Option<Receiver<Result<(), String>>>,
    /// True after a failure has been surfaced; suppresses repeat dialogs
    /// until a save succeeds again (repeat failures still log).
    failure_notified: bool,
}

impl AutosaveState {
    pub(crate) fn new() -> Self {
        Self {
            last_edit_version: 0,
            last_edit_at: None,
            saved_version: 0,
            in_flight: None,
            failure_notified: false,
        }
    }

    /// Record the latest content state. An edit is a `data_version` change
    /// observed while the project is dirty; project loads and undo-to-clean
    /// change the version with `is_dirty == false` and never arm the timer.
    pub(crate) fn observe(&mut self, data_version: u64, is_dirty: bool, now: Instant) {
        if is_dirty && data_version != self.last_edit_version {
            self.last_edit_version = data_version;
            self.last_edit_at = Some(now);
        }
    }

    /// True when a save should fire: armed, debounce elapsed, still dirty,
    /// nothing in flight, and the armed edit not already saved.
    pub(crate) fn should_fire(&self, is_dirty: bool, now: Instant, debounce: Duration) -> bool {
        self.in_flight.is_none()
            && is_dirty
            && self.last_edit_version != self.saved_version
            && self
                .last_edit_at
                .is_some_and(|t| now.duration_since(t) >= debounce)
    }

    /// Mark the armed edit as covered and adopt the worker's completion
    /// channel. Call exactly when the background save is spawned.
    pub(crate) fn begin(&mut self, rx: Receiver<Result<(), String>>) {
        self.saved_version = self.last_edit_version;
        self.last_edit_at = None;
        self.in_flight = Some(rx);
    }

    /// Drain a finished background save, if any. `None` while idle or still
    /// writing. A disconnected worker (spawn died) reports as an error.
    pub(crate) fn poll_completion(&mut self) -> Option<Result<(), String>> {
        let rx = self.in_flight.as_ref()?;
        match rx.try_recv() {
            Ok(result) => {
                self.in_flight = None;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.in_flight = None;
                Some(Err("autosave thread exited without reporting".to_string()))
            }
        }
    }

    /// One dialog per failure streak: returns true only for the first
    /// failure since the last success.
    pub(crate) fn note_failure(&mut self) -> bool {
        !std::mem::replace(&mut self.failure_notified, true)
    }

    pub(crate) fn note_success(&mut self) {
        self.failure_notified = false;
    }
}

/// UI-thread state stamped onto the snapshot clone before serialization —
/// the same fields `Application::save_viewport_state` writes for a manual
/// save (playhead, viewport, panel sizing, collapse states). Captured as
/// plain scalars so the background thread needs nothing from the UI.
pub(crate) struct UiStateStamp {
    saved_playhead_time: f32,
    viewport_scroll_x_beats: f32,
    viewport_scroll_y_px: f32,
    viewport_pixels_per_beat: f32,
    inspector_width: f32,
    timeline_height_percent: f32,
    macros_collapsed: bool,
    master_chrome_collapsed: bool,
    layer_chrome_collapsed: bool,
    clip_chrome_collapsed: bool,
}

impl UiStateStamp {
    fn apply(&self, project: &mut Project) {
        project.saved_playhead_time = self.saved_playhead_time;
        project.settings.viewport_scroll_x_beats = self.viewport_scroll_x_beats;
        project.settings.viewport_scroll_y_px = self.viewport_scroll_y_px;
        project.settings.viewport_pixels_per_beat = self.viewport_pixels_per_beat;
        project.settings.inspector_width = self.inspector_width;
        project.settings.timeline_height_percent = self.timeline_height_percent;
        project.settings.macros_collapsed = self.macros_collapsed;
        project.settings.master_chrome_collapsed = self.master_chrome_collapsed;
        project.settings.layer_chrome_collapsed = self.layer_chrome_collapsed;
        project.settings.clip_chrome_collapsed = self.clip_chrome_collapsed;
    }
}

/// Spawn the background save worker: clone the snapshot, stamp UI state,
/// save through the one existing save path with `is_auto = true`.
/// Returns the completion receiver, or an error if the thread can't spawn.
fn spawn_autosave(
    snapshot: Arc<Project>,
    path: PathBuf,
    stamp: UiStateStamp,
) -> Result<Receiver<Result<(), String>>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("autosave".to_string())
        .spawn(move || {
            // Deep clone on the background thread — the whole point: the UI
            // and content threads never pay for serialization (§6).
            let mut project = (*snapshot).clone();
            stamp.apply(&mut project);
            let result = manifold_io::saver::save_project(&mut project, &path, None, true)
                .map_err(|e| e.to_string());
            // Receiver gone = app shutting down; nothing to report to.
            let _ = tx.send(result);
        })
        .map_err(|e| format!("failed to spawn autosave thread: {e}"))?;
    Ok(rx)
}

impl Application {
    /// Per-frame autosave tick. Called from `tick_and_render` AFTER the
    /// content-state drain (so it sees the latest `data_version` /
    /// `editing_is_dirty`) and only in editor mode — perform mode returns
    /// before this point, which is exactly the D5 "timer parks" behavior.
    pub(crate) fn tick_autosave(&mut self) {
        // Finished background save → surface the result.
        if let Some(result) = self.autosave.poll_completion() {
            match result {
                Ok(()) => {
                    log::info!("[Autosave] Saved history snapshot");
                    self.autosave.note_success();
                    // New history entry → keep the revert menu current.
                    self.refresh_history_menu();
                }
                Err(e) => {
                    log::error!("[Autosave] Save failed: {e}");
                    if self.autosave.note_failure() {
                        crate::alerts::error(
                            "Autosave Failed",
                            &format!(
                                "MANIFOLD couldn't autosave the project:\n{e}\n\n\
                                 Your work is NOT safe on disk — check free space \
                                 and save manually (Cmd+S)."
                            ),
                        );
                    }
                }
            }
        }

        let now = Instant::now();
        self.autosave.observe(
            self.content_state.data_version,
            self.content_state.editing_is_dirty,
            now,
        );

        if !self
            .autosave
            .should_fire(self.content_state.editing_is_dirty, now, AUTOSAVE_DEBOUNCE)
        {
            return;
        }

        // Never journal mid-gesture: wait for the drag to finish (the
        // debounce re-check next frame costs nothing).
        if self.overlay.drag_mode() != manifold_ui::interaction_overlay::DragMode::None {
            return;
        }

        // Untitled project (no path yet) — nothing to save into. The timer
        // stays armed; the first Save As gives it a home.
        let Some(path) = self.current_project_path.clone() else {
            return;
        };
        // No snapshot received yet (content thread still warming up).
        let Some(snapshot) = self.last_snapshot_arc.clone() else {
            return;
        };

        let stamp = UiStateStamp {
            saved_playhead_time: self.content_state.current_time.as_f32(),
            viewport_scroll_x_beats: self.ws.ui_root.viewport.scroll_x_beats().as_f32(),
            viewport_scroll_y_px: self.ws.ui_root.viewport.scroll_y_px(),
            viewport_pixels_per_beat: self.ws.ui_root.viewport.pixels_per_beat(),
            inspector_width: self.ws.ui_root.layout.inspector_width,
            timeline_height_percent: self.ws.ui_root.layout.timeline_split_ratio,
            macros_collapsed: self.ws.ui_root.inspector.macros_panel().is_collapsed(),
            master_chrome_collapsed: self.ws.ui_root.inspector.master_chrome().is_collapsed(),
            layer_chrome_collapsed: self.ws.ui_root.inspector.layer_chrome().is_collapsed(),
            clip_chrome_collapsed: self.ws.ui_root.inspector.clip_chrome().is_collapsed(),
        };

        match spawn_autosave(snapshot, path, stamp) {
            Ok(rx) => {
                log::info!("[Autosave] Debounce elapsed — saving in background");
                self.autosave.begin(rx);
            }
            Err(e) => log::error!("[Autosave] {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBOUNCE: Duration = Duration::from_secs(60);

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn arms_on_dirty_edit_and_fires_after_debounce() {
        let mut s = AutosaveState::new();
        let start = t0();
        s.observe(1, true, start);
        assert!(!s.should_fire(true, start, DEBOUNCE), "no fire before debounce");
        assert!(
            !s.should_fire(true, start + Duration::from_secs(59), DEBOUNCE),
            "still inside debounce"
        );
        assert!(s.should_fire(true, start + DEBOUNCE, DEBOUNCE), "fires at debounce");
    }

    #[test]
    fn clean_version_changes_never_arm() {
        let mut s = AutosaveState::new();
        let start = t0();
        // Project load / undo-to-clean: version moves, dirty stays false.
        s.observe(5, false, start);
        assert!(!s.should_fire(false, start + DEBOUNCE * 2, DEBOUNCE));
        // Even if dirtiness appears later with no version change, stays disarmed.
        assert!(!s.should_fire(true, start + DEBOUNCE * 2, DEBOUNCE));
    }

    #[test]
    fn new_edit_resets_the_timer() {
        let mut s = AutosaveState::new();
        let start = t0();
        s.observe(1, true, start);
        // A second edit 30 s in pushes the deadline out.
        s.observe(2, true, start + Duration::from_secs(30));
        assert!(
            !s.should_fire(true, start + Duration::from_secs(60), DEBOUNCE),
            "debounce measures from the LAST edit"
        );
        assert!(s.should_fire(true, start + Duration::from_secs(90), DEBOUNCE));
    }

    #[test]
    fn no_wall_clock_refire_without_new_edit() {
        let mut s = AutosaveState::new();
        let start = t0();
        s.observe(1, true, start);
        assert!(s.should_fire(true, start + DEBOUNCE, DEBOUNCE));
        let (tx, rx) = std::sync::mpsc::channel();
        s.begin(rx);
        // Worker finishes successfully; completion drained.
        tx.send(Ok(())).unwrap();
        assert_eq!(s.poll_completion(), Some(Ok(())));
        // Hours pass, still dirty (autosave doesn't mark clean) — no re-fire.
        assert!(
            !s.should_fire(true, start + DEBOUNCE * 100, DEBOUNCE),
            "same edit burst must not save twice"
        );
        // A new edit re-arms.
        s.observe(2, true, start + DEBOUNCE * 100);
        assert!(s.should_fire(true, start + DEBOUNCE * 101, DEBOUNCE));
    }

    #[test]
    fn undo_to_clean_holds_fire_redo_rearms() {
        let mut s = AutosaveState::new();
        let start = t0();
        s.observe(1, true, start);
        // User undoes back to the saved state before the debounce elapses:
        // dirty false at fire time → hold.
        assert!(!s.should_fire(false, start + DEBOUNCE, DEBOUNCE));
        // Redo: dirty again with a version change → re-arms.
        s.observe(2, true, start + DEBOUNCE);
        assert!(s.should_fire(true, start + DEBOUNCE * 2, DEBOUNCE));
    }

    #[test]
    fn in_flight_blocks_and_completion_unblocks() {
        let mut s = AutosaveState::new();
        let start = t0();
        s.observe(1, true, start);
        let (tx, rx) = std::sync::mpsc::channel();
        s.begin(rx);
        s.observe(2, true, start + Duration::from_secs(1));
        assert!(
            !s.should_fire(true, start + DEBOUNCE * 2, DEBOUNCE),
            "no overlapping saves"
        );
        tx.send(Ok(())).unwrap();
        assert_eq!(s.poll_completion(), Some(Ok(())));
        assert!(s.should_fire(true, start + DEBOUNCE * 2, DEBOUNCE));
    }

    #[test]
    fn failure_notice_fires_once_per_streak() {
        let mut s = AutosaveState::new();
        assert!(s.note_failure(), "first failure notifies");
        assert!(!s.note_failure(), "repeat failures stay quiet");
        s.note_success();
        assert!(s.note_failure(), "a success re-arms the notice");
    }
}
