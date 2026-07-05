use crate::command::Command;
use manifold_core::project::Project;
use std::collections::VecDeque;

const MAX_UNDO_HISTORY: usize = 200;

/// Undo/redo manager with bounded history.
/// Port of C# UndoRedoManager.
pub struct UndoRedoManager {
    undo_stack: VecDeque<Box<dyn Command>>,
    redo_stack: Vec<Box<dyn Command>>,
}

impl UndoRedoManager {
    pub fn new() -> Self {
        Self {
            undo_stack: VecDeque::with_capacity(MAX_UNDO_HISTORY),
            redo_stack: Vec::with_capacity(32),
        }
    }

    /// Execute a command and push to undo stack.
    pub fn execute(&mut self, mut command: Box<dyn Command>, project: &mut Project) {
        command.execute(project);
        self.push_undo(command);
        self.redo_stack.clear();
    }

    /// Record an already-executed command (e.g., end of drag).
    pub fn record(&mut self, command: Box<dyn Command>) {
        self.push_undo(command);
        self.redo_stack.clear();
    }

    /// Undo the most recent command.
    #[must_use]
    pub fn undo(&mut self, project: &mut Project) -> bool {
        if let Some(mut cmd) = self.undo_stack.pop_back() {
            cmd.undo(project);
            self.redo_stack.push(cmd);
            true
        } else {
            false
        }
    }

    /// Redo the most recently undone command.
    #[must_use]
    pub fn redo(&mut self, project: &mut Project) -> bool {
        if let Some(mut cmd) = self.redo_stack.pop() {
            cmd.execute(project);
            self.undo_stack.push_back(cmd);
            // Cap undo stack
            while self.undo_stack.len() > MAX_UNDO_HISTORY {
                self.undo_stack.pop_front();
            }
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Description of the command `undo()` would act on next — the top of
    /// `undo_stack`. Read this BEFORE calling `undo()`: the command moves onto
    /// `redo_stack` once undone, so peeking after the fact would need a second
    /// accessor. Lets a caller (the D11 undo/redo toast,
    /// `UI_CRAFT_AND_MOTION_PLAN.md` P2) show the real command name — "Undo:
    /// Move Clip" — without exposing command objects themselves.
    pub fn peek_undo_description(&self) -> Option<&str> {
        self.undo_stack.back().map(|c| c.description())
    }

    /// Description of the command `redo()` would act on next — the top of
    /// `redo_stack`. Same "peek before mutating" contract as
    /// [`Self::peek_undo_description`].
    pub fn peek_redo_description(&self) -> Option<&str> {
        self.redo_stack.last().map(|c| c.description())
    }
    pub fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }
    pub fn redo_count(&self) -> usize {
        self.redo_stack.len()
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    fn push_undo(&mut self, command: Box<dyn Command>) {
        self.undo_stack.push_back(command);
        while self.undo_stack.len() > MAX_UNDO_HISTORY {
            self.undo_stack.pop_front();
        }
    }
}

impl Default for UndoRedoManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct SetBpmCommand {
        old_bpm: manifold_core::units::Bpm,
        new_bpm: manifold_core::units::Bpm,
    }

    impl Command for SetBpmCommand {
        fn execute(&mut self, project: &mut Project) {
            self.old_bpm = project.settings.bpm;
            project.settings.bpm = self.new_bpm;
        }
        fn undo(&mut self, project: &mut Project) {
            project.settings.bpm = self.old_bpm;
        }
        fn description(&self) -> &str {
            "Set BPM"
        }
    }

    #[test]
    fn test_undo_redo() {
        use manifold_core::units::Bpm;
        let mut mgr = UndoRedoManager::new();
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);

        mgr.execute(
            Box::new(SetBpmCommand {
                old_bpm: Bpm(0.0),
                new_bpm: Bpm(140.0),
            }),
            &mut project,
        );
        assert_eq!(project.settings.bpm, Bpm(140.0));
        assert!(mgr.can_undo());

        assert!(mgr.undo(&mut project));
        assert_eq!(project.settings.bpm, Bpm(120.0));
        assert!(mgr.can_redo());

        assert!(mgr.redo(&mut project));
        assert_eq!(project.settings.bpm, Bpm(140.0));
    }
}
