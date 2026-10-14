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

/// The context threaded through `dispatch`. Local `project` snapshot for
/// immediate UI feedback; `content_tx` carries authoritative mutations to the
/// content thread.
pub struct DispatchCtx<'a> {
    /// Local project snapshot mutated for immediate feedback.
    pub project: &'a mut Project,
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
    /// The UI's part stamps (D5). Only [`DispatchCtx::mutate`] and
    /// [`DispatchCtx::execute`] move them; a preview never does.
    pub stamps: &'a mut crate::frame_plan::UiStamps,
}

impl DispatchCtx<'_> {
    /// A non-undoable write: apply `f` to the local project, stamp `touches`,
    /// and send the same closure to the content thread (D5).
    pub fn mutate<F>(&mut self, touches: ChangeSet, f: F)
    where
        F: FnOnce(&mut Project) + Clone + Send + 'static,
    {
        (f.clone())(self.project);
        self.stamps.bump(touches);
        ContentCommand::send(self.content_tx, ContentCommand::mutate(touches, Box::new(f)));
    }

    /// An undoable edit: execute `cmd` on the local project, stamp what it
    /// touches, and send it to the content thread (D5).
    pub fn execute(&mut self, mut cmd: Box<dyn Command>) {
        cmd.execute(self.project);
        self.stamps.bump(cmd.touches());
        ContentCommand::send(self.content_tx, ContentCommand::Execute(cmd));
    }
}
