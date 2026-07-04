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
        Self {
            commands,
            desc: description,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Appends `to_append` to `Project::settings.video_library_paths` (a
    /// scratch `Vec<String>` field good enough to observe ordering) on
    /// execute, pops it back off on undo — cheap enough to prove
    /// `CompositeCommand`'s execute-forward/undo-reverse composition without
    /// any domain types.
    #[derive(Debug)]
    struct AppendCommand {
        to_append: String,
    }

    impl Command for AppendCommand {
        fn execute(&mut self, project: &mut Project) {
            project.settings.video_library_paths.push(self.to_append.clone());
        }
        fn undo(&mut self, project: &mut Project) {
            project.settings.video_library_paths.pop();
        }
        fn description(&self) -> &str {
            "Append"
        }
    }

    fn composite(items: &[&str]) -> CompositeCommand {
        let commands = items
            .iter()
            .map(|&s| Box::new(AppendCommand { to_append: s.to_string() }) as Box<dyn Command>)
            .collect();
        CompositeCommand::new(commands, "Append Many".to_string())
    }

    #[test]
    fn execute_applies_all_commands_in_order() {
        let mut project = Project::default();
        let mut cmd = composite(&["a", "b", "c"]);
        cmd.execute(&mut project);
        assert_eq!(project.settings.video_library_paths, vec!["a", "b", "c"]);
    }

    #[test]
    fn undo_reverses_all_commands_in_reverse_order() {
        // Each sub-command's undo only knows how to pop the LAST entry — if
        // CompositeCommand::undo ran forward instead of reverse, undoing 'a'
        // first (pop) would remove 'c' (the actual last entry), corrupting
        // the list instead of cleanly unwinding to empty.
        let mut project = Project::default();
        let mut cmd = composite(&["a", "b", "c"]);
        cmd.execute(&mut project);
        assert_eq!(project.settings.video_library_paths, vec!["a", "b", "c"]);

        cmd.undo(&mut project);
        assert!(
            project.settings.video_library_paths.is_empty(),
            "undo must reverse in the opposite order execute applied them"
        );
    }

    #[test]
    fn redo_reapplies_the_whole_group_as_one_unit() {
        let mut project = Project::default();
        let mut cmd = composite(&["x", "y"]);
        cmd.execute(&mut project);
        cmd.undo(&mut project);
        cmd.execute(&mut project);
        assert_eq!(
            project.settings.video_library_paths,
            vec!["x", "y"],
            "redo re-applies every sub-command"
        );
    }

    #[test]
    fn empty_command_list_is_a_no_op() {
        let mut project = Project::default();
        let mut cmd = CompositeCommand::new(Vec::new(), "Nothing".to_string());
        cmd.execute(&mut project);
        assert!(project.settings.video_library_paths.is_empty());
        cmd.undo(&mut project);
        assert!(project.settings.video_library_paths.is_empty());
    }
}
