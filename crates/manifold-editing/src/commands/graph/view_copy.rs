//! "Use this view" (SCENE_SURFACES D14): the viewport's view written into the
//! scene camera as one undo unit.

use crate::command::{Command, CompositeCommand};
use manifold_core::GraphTarget;
use manifold_core::project::Project;

/// The per-param camera writes, each built by the scene panel's one write
/// path (a bound param edits its card slot, an unbound one the graph), run
/// and undone together. The command is built once, on a click; nothing
/// writes the viewport camera into the project while it moves.
#[derive(Debug)]
pub struct CopyViewportCameraCommand {
    writes: CompositeCommand,
    empty: bool,
}

impl CopyViewportCameraCommand {
    pub fn new(writes: Vec<Box<dyn Command>>) -> Self {
        let empty = writes.is_empty();
        Self { writes: CompositeCommand::new(writes, "Use this view".into()), empty }
    }

    /// True when the scene camera already shows the view.
    pub fn is_empty(&self) -> bool {
        self.empty
    }
}

impl Command for CopyViewportCameraCommand {
    fn do_execute(&mut self, project: &mut Project) {
        self.writes.execute(project);
    }

    fn do_undo(&mut self, project: &mut Project) {
        self.writes.undo(project);
    }

    fn description(&self) -> &str {
        self.writes.description()
    }

    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        self.writes.graph_admission_targets(targets);
    }

    fn graph_admission_clips(&self, clips: &mut Vec<manifold_core::ClipId>) {
        self.writes.graph_admission_clips(clips);
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.writes.rejection_reason()
    }

    fn was_applied(&self) -> bool {
        self.writes.was_applied()
    }

    /// Card-slot writes touch values only; a write to an unbound camera
    /// param edits the graph and says so.
    fn touches(&self) -> manifold_core::change::ChangeSet {
        self.writes.touches()
    }
}
