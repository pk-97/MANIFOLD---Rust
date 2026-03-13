use crate::command::Command;
use manifold_core::project::Project;

// Placeholder — full effect commands will be ported in a later pass.
// Includes: AddEffect, RemoveEffect, ToggleEffect, ChangeEffectParam,
// GroupEffects, UngroupEffects, ReorderRack, etc.

/// Placeholder for effect commands.
#[derive(Debug)]
pub struct PlaceholderEffectCommand;

impl Command for PlaceholderEffectCommand {
    fn execute(&mut self, _project: &mut Project) {}
    fn undo(&mut self, _project: &mut Project) {}
    fn description(&self) -> &str { "Effect Command (placeholder)" }
}
