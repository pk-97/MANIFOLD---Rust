//! `DispatchCtx` — the read/write context every `dispatch` call threads
//! (UI_FUNNEL_DECOMPOSITION P-B, D3). Replaces `dispatch`'s eighteen positional
//! arguments with one borrow-struct: seven context borrows + `user_prefs` +
//! `editor_target`, plus `scrub` (the regrouped in-flight gesture snapshots,
//! `scrub.rs`). One owner per argument (adversarial review HIGH-2).
//!
//! Interior borrows, not owned data: `DispatchCtx` is built at each call site
//! from `Application`'s live fields and dropped when the dispatch returns. The
//! eight snapshot slots + `active_inspector_drag` ride as `scrub` VERBATIM until
//! P-I reshapes them into the addressed gesture engine (D4).

use crate::app::SelectionState;
use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::ui_root::UIRoot;
use crate::user_prefs::UserPrefs;
use manifold_core::change::ChangeSet;
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId};
use manifold_editing::command::{rows_deferred, rows_written, Command};

use super::scrub::ScrubState;

/// The UI's copy of the project (UI_INVALIDATION D5). Readable through
/// `Deref`; writable only through [`Self::edit`], a [`ProjectRef`] that stamps
/// what it writes, or [`Self::intake`], which the snapshot accept and project
/// load use because their stamps come from the snapshot itself. An unstamped
/// edit does not compile.
pub struct LocalProject(Project);

impl LocalProject {
    pub fn new(project: Project) -> Self {
        Self(project)
    }

    /// The one way to edit the local project.
    pub fn edit<'a>(
        &'a mut self,
        edits: &'a mut crate::gesture_preview::UiEdits,
        content_tx: &'a crossbeam_channel::Sender<ContentCommand>,
    ) -> ProjectRef<'a> {
        ProjectRef::new(&mut self.0, edits, content_tx)
    }

    /// Writes that arrive from the content thread (snapshot accept, live
    /// values, a drag's held value) or replace the project (load, new): the
    /// accepted snapshot's stamps already cover them. Never an edit.
    pub fn intake(&mut self) -> &mut Project {
        &mut self.0
    }

    /// Clips active at `beat`. It refreshes the timeline's sort cache, which
    /// is not model state, so it needs no stamp.
    pub fn active_clips_at_beat(&mut self, beat: manifold_core::Beats, results: &mut Vec<(usize, usize)>, indices: &mut Vec<usize>) {
        self.0.timeline.get_active_clips_at_beat(beat, results, indices);
    }
}

impl std::ops::Deref for LocalProject {
    type Target = Project;
    fn deref(&self) -> &Project {
        &self.0
    }
}

impl std::borrow::Borrow<Project> for LocalProject {
    fn borrow(&self) -> &Project {
        &self.0
    }
}

/// The local project as dispatch sees it: readable through `Deref`, writable
/// only through [`ProjectRef::mutate`] and [`ProjectRef::execute`], which stamp
/// the parts they touch and send the same edit to the content thread
/// (UI_INVALIDATION D5). There is no other way to write it.
pub struct ProjectRef<'a> {
    project: &'a mut Project,
    edits: &'a mut crate::gesture_preview::UiEdits,
    content_tx: &'a crossbeam_channel::Sender<ContentCommand>,
}

impl<'a> ProjectRef<'a> {
    pub(crate) fn new(
        project: &'a mut Project,
        edits: &'a mut crate::gesture_preview::UiEdits,
        content_tx: &'a crossbeam_channel::Sender<ContentCommand>,
    ) -> Self {
        Self { project, edits, content_tx }
    }

    /// A non-undoable write: apply `f` locally, stamp what it wrote (as
    /// `EditingService::mutate` does), and send the same closure to the
    /// content thread.
    pub fn mutate<F>(&mut self, touches: ChangeSet, f: F)
    where
        F: FnOnce(&mut Project) + Clone + Send + 'static,
    {
        let ((), rows) = rows_written(self.project, f.clone());
        self.edits.stamps.bump(touches.union(rows));
        ContentCommand::send(self.content_tx, ContentCommand::mutate(touches, Box::new(f)));
    }

    /// A gesture's live move (a scrub tick): as [`Self::mutate`], but rows
    /// wait for the gesture's commit on both threads, so no row appears or
    /// vanishes under the cursor mid-drag.
    pub fn gesture_move<F>(&mut self, touches: ChangeSet, f: F)
    where
        F: FnOnce(&mut Project) + Clone + Send + 'static,
    {
        rows_deferred(self.project, f.clone());
        self.edits.stamps.bump(touches);
        ContentCommand::send(self.content_tx, ContentCommand::gesture_move(touches, Box::new(f)));
    }

    /// An undoable edit: execute `cmd` locally, stamp what it touches (read
    /// after execute, so a command can learn its parts from the project), and
    /// send it to the content thread.
    pub fn execute(&mut self, mut cmd: Box<dyn Command>) {
        let wrote = cmd.execute(self.project);
        self.edits.stamps.bump(wrote);
        ContentCommand::send(self.content_tx, ContentCommand::Execute(cmd));
    }

    /// Several undoable edits as one undo step, stamped with their union.
    /// They run as one composite, as the content thread runs them, so a run
    /// of scene edits compiles once here too.
    pub fn execute_batch(&mut self, cmds: Vec<Box<dyn Command>>, label: String) {
        let mut batch = manifold_editing::command::CompositeCommand::new(cmds, label.clone());
        let wrote = batch.execute(self.project);
        self.edits.stamps.bump(wrote);
        ContentCommand::send(self.content_tx, ContentCommand::ExecuteBatch(batch.into_commands(), label));
    }

    /// For factories that build a command and execute it on the project they
    /// are handed (`EditingService::create_clip_at_position` and kin): stamp
    /// what the returned command wrote and send it.
    pub fn executed_by<R>(
        &mut self,
        f: impl FnOnce(&mut Project) -> Option<(Box<dyn Command>, R)>,
    ) -> Option<R> {
        let (made, rows) = rows_written(self.project, f);
        let (cmd, out) = made?;
        self.edits.stamps.bump(cmd.touches().union(rows));
        ContentCommand::send(self.content_tx, ContentCommand::Execute(cmd));
        Some(out)
    }

    /// For helpers that write the local project and send their own content
    /// commands (the keyboard automation helpers, the file importers): `f`
    /// returns what those commands touch, and that is stamped.
    pub fn with_sent<R>(&mut self, f: impl FnOnce(&mut Project) -> (R, ChangeSet)) -> R {
        let ((out, touches), rows) = rows_written(self.project, f);
        self.edits.stamps.bump(touches.union(rows));
        out
    }

    /// Stamp an edit the local project already shows (a committed gesture
    /// preview) or one only the content thread runs, as its command is sent.
    pub fn stamp(&mut self, touches: ChangeSet) {
        self.edits.stamps.bump(touches);
    }

    /// A gesture preview write: local only, never sent, stamping nothing; it
    /// records itself in the gesture preview, whose version the planner reads
    /// (D12). The gesture's commit stamps.
    pub fn preview<R>(&mut self, f: impl FnOnce(&mut Project, &mut crate::gesture_preview::GesturePreview) -> R) -> R {
        f(self.project, &mut self.edits.preview)
    }

    /// The gesture preview, read or reset without a project write.
    pub fn gesture_preview(&mut self) -> &mut crate::gesture_preview::GesturePreview {
        &mut self.edits.preview
    }

    pub fn content_tx(&self) -> &'a crossbeam_channel::Sender<ContentCommand> {
        self.content_tx
    }

    /// A shorter-lived writer over the same project, for one dispatch in a loop.
    pub fn reborrow(&mut self) -> ProjectRef<'_> {
        ProjectRef { project: self.project, edits: self.edits, content_tx: self.content_tx }
    }

    /// Clip lookup by id. It refreshes the timeline's id cache, which is not
    /// model state, so it needs no stamp.
    pub fn find_clip_by_id(&mut self, clip_id: &str) -> Option<&manifold_core::clip::TimelineClip> {
        self.project.timeline.find_clip_by_id(clip_id)
    }
}

impl std::ops::Deref for ProjectRef<'_> {
    type Target = Project;
    fn deref(&self) -> &Project {
        self.project
    }
}

/// The context threaded through `dispatch`. Local `project` snapshot for
/// immediate UI feedback; `content_tx` carries authoritative mutations to the
/// content thread.
pub struct DispatchCtx<'a> {
    /// Local project snapshot, read-only; its `mutate`/`execute` write it.
    pub project: ProjectRef<'a>,
    /// Authoritative-mutation channel to the content thread.
    pub content_tx: &'a crossbeam_channel::Sender<ContentCommand>,
    /// Latest content-thread snapshot (read-only).
    pub content_state: &'a ContentState,
    /// The window's UI tree + panel state.
    pub ui: &'a mut UIRoot,
    /// Timeline / inspector selection.
    pub selection: &'a mut SelectionState,
    /// The inspector's focused layer.
    pub active_layer: &'a mut Option<LayerId>,
    /// User preferences (persisted UI toggles).
    pub user_prefs: &'a mut UserPrefs,
    /// `Some` when the graph editor dispatches a left-lane card action against
    /// an effect/generator by stable identity; `None` on the inspector/perform
    /// path. Only consulted by the inspector handlers.
    pub editor_target: Option<&'a GraphTarget>,
    /// In-flight scrub-gesture snapshots (regrouped; see `scrub.rs`).
    pub scrub: &'a mut ScrubState,
    /// Parent window and project path for any file dialog an action opens.
    /// Default (floating, no project) for headless callers.
    pub dialogs: crate::file_dialogs::DialogHost<'a>,
}

impl DispatchCtx<'_> {
    /// An export owns the project: the content thread drops every edit sent
    /// meanwhile (BUG-v2o9y), so applying one locally would leave the UI
    /// showing a change the show never took. Refused here for every window
    /// and shortcut that dispatches.
    fn export_locks_edits(&self) -> bool {
        self.content_state.is_exporting
    }

    /// See [`ProjectRef::mutate`]. Refused during an export.
    pub fn mutate<F>(&mut self, touches: ChangeSet, f: F)
    where
        F: FnOnce(&mut Project) + Clone + Send + 'static,
    {
        if self.export_locks_edits() {
            return;
        }
        self.project.mutate(touches, f);
    }

    /// See [`ProjectRef::execute`]. Refused during an export.
    pub fn execute(&mut self, cmd: Box<dyn Command>) {
        if self.export_locks_edits() {
            return;
        }
        self.project.execute(cmd);
    }

    /// See [`ProjectRef::execute_batch`]. Refused during an export.
    pub fn execute_batch(&mut self, cmds: Vec<Box<dyn Command>>, label: String) {
        if self.export_locks_edits() {
            return;
        }
        self.project.execute_batch(cmds, label);
    }

    /// See [`ProjectRef::gesture_move`]. Refused during an export.
    pub fn gesture_move<F>(&mut self, touches: ChangeSet, f: F)
    where
        F: FnOnce(&mut Project) + Clone + Send + 'static,
    {
        if self.export_locks_edits() {
            return;
        }
        self.project.gesture_move(touches, f);
    }
}

#[cfg(test)]
mod export_lock_tests {
    use super::*;

    /// BUG-v2o9y: during an export an edit is refused in the UI copy and
    /// never sent; outside one it applies and is sent.
    #[test]
    fn export_refuses_edits_in_dispatch_ctx() {
        for (exporting, applied) in [(true, false), (false, true)] {
            let mut project = Project::default();
            let (content_tx, content_rx) = crossbeam_channel::unbounded();
            let mut edits = Default::default();
            let content_state = ContentState { is_exporting: exporting, ..ContentState::default() };
            let mut ui = UIRoot::new();
            let mut selection = manifold_ui::UIState::new();
            let mut active_layer = None;
            let mut user_prefs = UserPrefs::in_memory();
            let mut scrub = ScrubState::default();
            let mut ctx = DispatchCtx {
                project: ProjectRef::new(&mut project, &mut edits, &content_tx),
                content_tx: &content_tx,
                content_state: &content_state,
                ui: &mut ui,
                selection: &mut selection,
                active_layer: &mut active_layer,
                user_prefs: &mut user_prefs,
                editor_target: None,
                scrub: &mut scrub,
                dialogs: Default::default(),
            };
            ctx.mutate(crate::content_command::touch::VALUES, |p| p.settings.frame_rate = 24.0);
            let sent_mutate = content_rx.try_iter().count();
            ctx.execute(Box::new(manifold_editing::commands::settings::ChangeTonemapCurveCommand::new(
                manifold_core::TonemapCurve::default(),
                false,
                manifold_core::TonemapCurve::default(),
                true,
            )));
            let sent_execute = content_rx.try_iter().count();
            assert_eq!(ctx.project.settings.frame_rate == 24.0, applied, "exporting={exporting}");
            assert_eq!(ctx.project.settings.tonemap_enabled, applied, "exporting={exporting}");
            assert_eq!((sent_mutate, sent_execute), (usize::from(applied), usize::from(applied)), "exporting={exporting}");
        }
    }
}
