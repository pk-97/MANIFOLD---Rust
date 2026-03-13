use crate::command::Command;
use manifold_core::project::Project;

// Placeholder — full layer commands will be ported in a later pass.
// The core patterns (AddLayer, DeleteLayer, ReorderLayer, Group/Ungroup)
// follow the same snapshot+restore approach as clip commands.

/// Placeholder for layer commands.
#[derive(Debug)]
pub struct PlaceholderLayerCommand;

impl Command for PlaceholderLayerCommand {
    fn execute(&mut self, _project: &mut Project) {}
    fn undo(&mut self, _project: &mut Project) {}
    fn description(&self) -> &str { "Layer Command (placeholder)" }
}
