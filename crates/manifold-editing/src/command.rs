use manifold_core::layer::Layer;
use manifold_core::project::Project;
use std::fmt::Debug;

/// Trait for undoable commands. Port of C# ICommand.
pub trait Command: Debug + Send {
    fn execute(&mut self, project: &mut Project);
    fn undo(&mut self, project: &mut Project);
    fn description(&self) -> &str;
}

/// Callbacks for layer lifecycle events (add/remove).
/// Port of C# ILayerLifecycleCallbacks.cs lines 10-14.
/// Used by layer add/delete commands to notify UI/compositing for
/// OSC registration, effect cleanup, etc.
pub trait LayerLifecycleCallbacks {
    fn on_layer_added(&mut self, layer: &Layer);
    fn on_layer_removed(&mut self, layer: &Layer);
}

/// Composite command that groups multiple commands.
/// Execute all in order, undo all in reverse.
#[derive(Debug)]
pub struct CompositeCommand {
    commands: Vec<Box<dyn Command>>,
    desc: String,
}

impl CompositeCommand {
    pub fn new(commands: Vec<Box<dyn Command>>, description: String) -> Self {
        Self { commands, desc: description }
    }
}

impl Command for CompositeCommand {
    fn execute(&mut self, project: &mut Project) {
        for cmd in &mut self.commands {
            cmd.execute(project);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        for cmd in self.commands.iter_mut().rev() {
            cmd.undo(project);
        }
    }

    fn description(&self) -> &str {
        &self.desc
    }
}
