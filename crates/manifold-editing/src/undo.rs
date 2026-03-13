use manifold_core::project::Project;
use crate::command::Command;
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

    pub fn can_undo(&self) -> bool { !self.undo_stack.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo_stack.is_empty() }
    pub fn undo_count(&self) -> usize { self.undo_stack.len() }
    pub fn redo_count(&self) -> usize { self.redo_stack.len() }

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
        old_bpm: f32,
        new_bpm: f32,
    }

    impl Command for SetBpmCommand {
        fn execute(&mut self, project: &mut Project) {
            self.old_bpm = project.settings.bpm;
            project.settings.bpm = self.new_bpm;
        }
        fn undo(&mut self, project: &mut Project) {
            project.settings.bpm = self.old_bpm;
        }
        fn description(&self) -> &str { "Set BPM" }
    }

    #[test]
    fn test_undo_redo() {
        let mut mgr = UndoRedoManager::new();
        let mut project = Project::default();
        project.settings.bpm = 120.0;

        mgr.execute(Box::new(SetBpmCommand { old_bpm: 0.0, new_bpm: 140.0 }), &mut project);
        assert_eq!(project.settings.bpm, 140.0);
        assert!(mgr.can_undo());

        mgr.undo(&mut project);
        assert_eq!(project.settings.bpm, 120.0);
        assert!(mgr.can_redo());

        mgr.redo(&mut project);
        assert_eq!(project.settings.bpm, 140.0);
    }
}
