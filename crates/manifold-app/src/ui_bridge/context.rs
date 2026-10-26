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
use manifold_editing::command::Command;
use std::sync::Arc;

use super::scrub::ScrubState;

/// The UI's view of the project: the content thread's published `Arc<Project>`,
/// read-only. Readable through `Deref`; an edit is sent through
/// [`Self::edit`]'s [`ProjectRef`], which stamps it and never writes the
/// project. The view changes only when [`Self::show`] swaps in a newer
/// snapshot.
pub struct LocalProject(Arc<Project>);

impl LocalProject {
    /// A view over `project` with its timeline caches fresh, as a published
    /// snapshot's are.
    pub fn new(mut project: Project) -> Self {
        project.timeline.refresh_caches();
        Self(Arc::new(project))
    }

    /// The one way to send an edit for the local project.
    pub fn edit<'a>(
        &'a mut self,
        edits: &'a mut crate::gesture_preview::UiEdits,
        content_tx: &'a crossbeam_channel::Sender<ContentCommand>,
    ) -> ProjectRef<'a> {
        ProjectRef::new(&self.0, edits, content_tx)
    }

    /// Show the content thread's published project. Returns false when it is
    /// the allocation already shown (modulation-only frames resend it).
    pub fn show(&mut self, snapshot: Arc<Project>) -> bool {
        if Arc::ptr_eq(&self.0, &snapshot) {
            return false;
        }
        self.0 = snapshot;
        true
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

/// The UI's read-only view of the project (the content thread's published
/// copy) plus the channel edits leave on. `mutate`, `execute` and
/// `execute_batch` only send: the content thread runs each edit once and the
/// next snapshot brings the result and its stamps. A drag writes only the
/// gesture preview.
pub struct ProjectRef<'a> {
    project: &'a Project,
    edits: &'a mut crate::gesture_preview::UiEdits,
    content_tx: &'a crossbeam_channel::Sender<ContentCommand>,
}

impl<'a> ProjectRef<'a> {
    pub(crate) fn new(
        project: &'a Project,
        edits: &'a mut crate::gesture_preview::UiEdits,
        content_tx: &'a crossbeam_channel::Sender<ContentCommand>,
    ) -> Self {
        Self {
            project,
            edits,
            content_tx,
        }
    }

    /// A non-undoable write, sent to the content thread to run through
    /// `EditingService::mutate`.
    pub fn mutate<F>(&mut self, touches: ChangeSet, f: F)
    where
        F: FnOnce(&mut Project) + Send + 'static,
    {
        ContentCommand::send(
            self.content_tx,
            ContentCommand::mutate(touches, Box::new(f)),
        );
    }

    /// A gesture's live move (a scrub tick), sent to the content thread
    /// only. The UI's copy keeps the pre-drag value and controls draw the
    /// drag from `ScrubState`. Rows wait for the gesture's commit on the
    /// content thread.
    pub fn gesture_move<F>(&mut self, touches: ChangeSet, f: F)
    where
        F: FnOnce(&mut Project) + Clone + Send + 'static,
    {
        self.edits.stamps.bump(touches);
        ContentCommand::send(
            self.content_tx,
            ContentCommand::gesture_move(touches, Box::new(f)),
        );
    }

    /// An undoable edit, sent to the content thread.
    pub fn execute(&mut self, cmd: Box<dyn Command>) {
        ContentCommand::send(self.content_tx, ContentCommand::Execute(cmd));
    }

    /// Several undoable edits as one undo step.
    pub fn execute_batch(&mut self, cmds: Vec<Box<dyn Command>>, label: String) {
        ContentCommand::send(self.content_tx, ContentCommand::ExecuteBatch(cmds, label));
    }

    /// A gesture preview gained or lost a whole lane: the panels re-project
    /// it, as they do when the committed edit lands.
    pub fn preview_shape_changed(&mut self, touches: ChangeSet) {
        self.edits.stamps.bump(touches);
    }

    /// The gesture preview, for reads that lay the dragged state over the
    /// project.
    pub fn preview(&self) -> &crate::gesture_preview::GesturePreview {
        &self.edits.preview
    }

    /// The project to read and the gesture preview to write. A drag records
    /// into the preview; it never writes the project.
    pub fn preview_mut(&mut self) -> (&Project, &mut crate::gesture_preview::GesturePreview) {
        (self.project, &mut self.edits.preview)
    }

    pub fn content_tx(&self) -> &'a crossbeam_channel::Sender<ContentCommand> {
        self.content_tx
    }

    /// A shorter-lived view over the same project, for one dispatch in a loop.
    pub fn reborrow(&mut self) -> ProjectRef<'_> {
        ProjectRef {
            project: self.project,
            edits: self.edits,
            content_tx: self.content_tx,
        }
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
    /// The content thread's published project, read-only; its `mutate` and
    /// `execute` send edits.
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
    /// meanwhile, so one sent now would only be lost. Refused here for every
    /// window and shortcut that dispatches.
    fn export_locks_edits(&self) -> bool {
        self.content_state.is_exporting
    }

    /// See [`ProjectRef::mutate`]. Refused during an export.
    pub fn mutate<F>(&mut self, touches: ChangeSet, f: F)
    where
        F: FnOnce(&mut Project) + Send + 'static,
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

    /// During an export an edit is refused and never sent; outside one it is
    /// sent. The UI's copy changes in neither case.
    #[test]
    fn export_refuses_edits_in_dispatch_ctx() {
        for (exporting, sent) in [(true, false), (false, true)] {
            let project = Project::default();
            let (content_tx, content_rx) = crossbeam_channel::unbounded();
            let mut edits = Default::default();
            let content_state = ContentState {
                is_exporting: exporting,
                ..ContentState::default()
            };
            let mut ui = UIRoot::new();
            let mut selection = manifold_ui::UIState::new();
            let mut active_layer = None;
            let mut user_prefs = UserPrefs::in_memory();
            let mut scrub = ScrubState::default();
            let mut ctx = DispatchCtx {
                project: ProjectRef::new(&project, &mut edits, &content_tx),
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
            ctx.mutate(crate::content_command::touch::VALUES, |p| {
                p.settings.frame_rate = 24.0
            });
            let sent_mutate = content_rx.try_iter().count();
            ctx.execute(Box::new(
                manifold_editing::commands::settings::ChangeTonemapCurveCommand::new(
                    manifold_core::TonemapCurve::default(),
                    false,
                    manifold_core::TonemapCurve::default(),
                    true,
                ),
            ));
            let sent_execute = content_rx.try_iter().count();
            assert_ne!(ctx.project.settings.frame_rate, 24.0);
            assert!(!ctx.project.settings.tonemap_enabled);
            assert_eq!(
                (sent_mutate, sent_execute),
                (usize::from(sent), usize::from(sent)),
                "exporting={exporting}"
            );
        }
    }
}
