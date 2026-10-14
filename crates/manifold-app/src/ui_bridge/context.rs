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

use super::scrub::ScrubState;

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
    pub fn new(
        project: &'a mut Project,
        edits: &'a mut crate::gesture_preview::UiEdits,
        content_tx: &'a crossbeam_channel::Sender<ContentCommand>,
    ) -> Self {
        Self { project, edits, content_tx }
    }

    /// A non-undoable write: apply `f` locally, stamp `touches`, and send the
    /// same closure to the content thread.
    pub fn mutate<F>(&mut self, touches: ChangeSet, f: F)
    where
        F: FnOnce(&mut Project) + Clone + Send + 'static,
    {
        (f.clone())(self.project);
        self.edits.stamps.bump(touches);
        ContentCommand::send(self.content_tx, ContentCommand::mutate(touches, Box::new(f)));
    }

    /// An undoable edit: execute `cmd` locally, stamp what it touches (read
    /// after execute, so a command can learn its parts from the project), and
    /// send it to the content thread.
    pub fn execute(&mut self, mut cmd: Box<dyn Command>) {
        cmd.execute(self.project);
        self.edits.stamps.bump(cmd.touches());
        ContentCommand::send(self.content_tx, ContentCommand::Execute(cmd));
    }

    /// Several undoable edits as one undo step, stamped with their union.
    pub fn execute_batch(&mut self, mut cmds: Vec<Box<dyn Command>>, label: String) {
        let mut touches = ChangeSet::NONE;
        for cmd in &mut cmds {
            cmd.execute(self.project);
            touches = touches.union(cmd.touches());
        }
        self.edits.stamps.bump(touches);
        ContentCommand::send(self.content_tx, ContentCommand::ExecuteBatch(cmds, label));
    }

    /// For factories that build a command and execute it on the project they
    /// are handed (`EditingService::create_clip_at_position` and kin): stamp
    /// what the returned command touches and send it.
    pub fn executed_by<R>(
        &mut self,
        f: impl FnOnce(&mut Project) -> Option<(Box<dyn Command>, R)>,
    ) -> Option<R> {
        let (cmd, out) = f(self.project)?;
        self.edits.stamps.bump(cmd.touches());
        ContentCommand::send(self.content_tx, ContentCommand::Execute(cmd));
        Some(out)
    }

    /// For helpers shared with the keyboard path that write the local project
    /// and send their own content command: stamp `touches` (what that command
    /// declares) around the call.
    pub fn with_sent<R>(&mut self, touches: ChangeSet, f: impl FnOnce(&mut Project) -> R) -> R {
        self.edits.stamps.bump(touches);
        f(self.project)
    }

    /// Clip lookup by id. It refreshes the timeline's id cache, which is not
    /// model state, so it needs no stamp.
    pub fn find_clip_by_id(&mut self, clip_id: &str) -> Option<&manifold_core::clip::TimelineClip> {
        self.project.timeline.find_clip_by_id(clip_id)
    }

    /// A marker drag preview: a local-only write the content thread never sees
    /// and that stamps nothing (D12). It moves the marker preview version; the
    /// gesture's commit stamps Markers.
    pub fn preview_marker<R>(&mut self, f: impl FnOnce(&mut Project) -> R) -> R {
        let out = f(self.project);
        self.edits.preview.record_marker();
        out
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
    /// See [`ProjectRef::mutate`].
    pub fn mutate<F>(&mut self, touches: ChangeSet, f: F)
    where
        F: FnOnce(&mut Project) + Clone + Send + 'static,
    {
        self.project.mutate(touches, f);
    }

    /// See [`ProjectRef::execute`].
    pub fn execute(&mut self, cmd: Box<dyn Command>) {
        self.project.execute(cmd);
    }
}
