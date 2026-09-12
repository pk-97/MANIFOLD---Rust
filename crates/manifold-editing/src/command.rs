use manifold_core::GraphTarget;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use std::fmt::Debug;

/// Trait for undoable commands. Port of C# ICommand.
pub trait Command: Debug + Send {
    fn execute(&mut self, project: &mut Project);
    fn undo(&mut self, project: &mut Project);
    fn description(&self) -> &str;
    /// Append graph owners whose structural edits require canonical admission.
    /// Live value writes intentionally leave this empty.
    fn graph_admission_targets(&self, _targets: &mut Vec<GraphTarget>) {}
    /// Clip-scoped source edits resolve their generator owner at execution.
    fn graph_admission_clips(&self, _clips: &mut Vec<manifold_core::ClipId>) {}
    /// Optional stable diagnostic when execution or admission was rejected.
    fn rejection_reason(&self) -> Option<&str> {
        None
    }
    /// Commands that can reject a stale prepared edit report whether execute
    /// actually changed the project. Existing commands retain their behavior.
    fn was_applied(&self) -> bool {
        true
    }
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
    rejection: Option<String>,
}

impl CompositeCommand {
    pub fn new(commands: Vec<Box<dyn Command>>, description: String) -> Self {
        Self {
            commands,
            desc: description,
            rejection: None,
        }
    }
}

impl Command for CompositeCommand {
    fn execute(&mut self, project: &mut Project) {
        self.rejection = None;
        for index in 0..self.commands.len() {
            self.commands[index].execute(project);
            if let Some(reason) = self.commands[index].rejection_reason() {
                self.rejection = Some(reason.to_string());
                for previous in self.commands[..index]
                    .iter_mut()
                    .rev()
                    .filter(|command| command.was_applied())
                {
                    previous.undo(project);
                }
                return;
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if self.rejection.is_some() {
            return;
        }
        for cmd in self
            .commands
            .iter_mut()
            .rev()
            .filter(|cmd| cmd.was_applied())
        {
            cmd.undo(project);
        }
    }

    fn description(&self) -> &str {
        &self.desc
    }

    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        for command in &self.commands {
            command.graph_admission_targets(targets);
        }
    }

    fn graph_admission_clips(&self, clips: &mut Vec<manifold_core::ClipId>) {
        for command in &self.commands {
            command.graph_admission_clips(clips);
        }
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }

    fn was_applied(&self) -> bool {
        self.rejection.is_none() && self.commands.iter().any(|command| command.was_applied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TargetCommand(GraphTarget);

    impl Command for TargetCommand {
        fn execute(&mut self, _project: &mut Project) {}
        fn undo(&mut self, _project: &mut Project) {}
        fn description(&self) -> &str {
            "Target"
        }
        fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
            targets.push(self.0.clone());
        }
    }

    #[derive(Debug)]
    struct ReasonCommand(&'static str);

    impl Command for ReasonCommand {
        fn execute(&mut self, _project: &mut Project) {}
        fn undo(&mut self, _project: &mut Project) {}
        fn description(&self) -> &str {
            "Reason"
        }
        fn rejection_reason(&self) -> Option<&str> {
            Some(self.0)
        }
    }

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
            project
                .settings
                .video_library_paths
                .push(self.to_append.clone());
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
            .map(|&s| {
                Box::new(AppendCommand {
                    to_append: s.to_string(),
                }) as Box<dyn Command>
            })
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

    #[test]
    fn composite_forwards_graph_admission_targets() {
        let first = GraphTarget::Generator(manifold_core::LayerId::new("first"));
        let second = GraphTarget::Generator(manifold_core::LayerId::new("second"));
        let command = CompositeCommand::new(
            vec![
                Box::new(TargetCommand(first.clone())),
                Box::new(TargetCommand(second.clone())),
            ],
            "Targets".into(),
        );
        let mut targets = Vec::new();
        command.graph_admission_targets(&mut targets);
        assert_eq!(targets, vec![first, second]);
    }

    #[test]
    fn composite_rejection_rolls_back_prior_writes_and_skips_later_children() {
        let mut project = Project::default();
        let mut command = CompositeCommand::new(
            vec![
                Box::new(AppendCommand {
                    to_append: "before".into(),
                }),
                Box::new(ReasonCommand("invalid source")),
                Box::new(AppendCommand {
                    to_append: "after".into(),
                }),
            ],
            "Atomic edit".into(),
        );
        command.execute(&mut project);
        assert!(project.settings.video_library_paths.is_empty());
        assert!(!command.was_applied());
        command.undo(&mut project);
        assert!(project.settings.video_library_paths.is_empty());
    }

    #[test]
    fn composite_reports_first_child_rejection_reason() {
        let mut command = CompositeCommand::new(
            vec![
                Box::new(ReasonCommand("first")),
                Box::new(ReasonCommand("second")),
            ],
            "Reasons".into(),
        );
        command.execute(&mut Project::default());
        assert_eq!(command.rejection_reason(), Some("first"));
    }
}
