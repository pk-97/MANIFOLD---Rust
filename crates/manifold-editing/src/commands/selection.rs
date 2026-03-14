use crate::command::Command;
use manifold_core::project::Project;
use manifold_core::selection::SelectionRegion;

/// Set the selection region. Operates on a SelectionState stored on Project
/// (added as a runtime-only field).
#[derive(Debug)]
pub struct SetSelectionRegionCommand {
    old_region: SelectionRegion,
    new_region: SelectionRegion,
}

impl SetSelectionRegionCommand {
    pub fn new(old_region: SelectionRegion, new_region: SelectionRegion) -> Self {
        Self { old_region, new_region }
    }
}

impl Command for SetSelectionRegionCommand {
    fn execute(&mut self, _project: &mut Project) {
        // Selection is UI state — stored externally, not on Project.
        // This command exists for undo/redo tracking of selection changes.
        // The actual selection state is managed by EditingService.
    }

    fn undo(&mut self, _project: &mut Project) {
        // Selection restore handled by EditingService reading old_region.
    }

    fn description(&self) -> &str { "Set Selection" }
}

impl SetSelectionRegionCommand {
    pub fn old_region(&self) -> &SelectionRegion {
        &self.old_region
    }

    pub fn new_region(&self) -> &SelectionRegion {
        &self.new_region
    }
}
