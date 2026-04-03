use crate::command::Command;
use manifold_core::id::MarkerId;
use manifold_core::marker::TimelineMarker;
use manifold_core::project::Project;
use manifold_core::units::Beats;

// ── Add Marker ──────────────────────────────────────────────────

#[derive(Debug)]
pub struct AddMarkerCommand {
    marker: TimelineMarker,
}

impl AddMarkerCommand {
    pub fn new(marker: TimelineMarker) -> Self {
        Self { marker }
    }
}

impl Command for AddMarkerCommand {
    fn execute(&mut self, project: &mut Project) {
        project.timeline.add_marker(self.marker.clone());
    }

    fn undo(&mut self, project: &mut Project) {
        project.timeline.remove_marker(&self.marker.id);
    }

    fn description(&self) -> &str {
        "Add Marker"
    }
}

// ── Delete Marker ───────────────────────────────────────────────

#[derive(Debug)]
pub struct DeleteMarkerCommand {
    marker_id: MarkerId,
    removed: Option<TimelineMarker>,
}

impl DeleteMarkerCommand {
    pub fn new(marker_id: MarkerId) -> Self {
        Self {
            marker_id,
            removed: None,
        }
    }
}

impl Command for DeleteMarkerCommand {
    fn execute(&mut self, project: &mut Project) {
        self.removed = project.timeline.remove_marker(&self.marker_id);
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(marker) = self.removed.take() {
            project.timeline.add_marker(marker);
        }
    }

    fn description(&self) -> &str {
        "Delete Marker"
    }
}

// ── Move Marker ─────────────────────────────────────────────────

#[derive(Debug)]
pub struct MoveMarkerCommand {
    marker_id: MarkerId,
    old_beat: Beats,
    new_beat: Beats,
}

impl MoveMarkerCommand {
    pub fn new(marker_id: MarkerId, old_beat: Beats, new_beat: Beats) -> Self {
        Self {
            marker_id,
            old_beat,
            new_beat,
        }
    }
}

impl Command for MoveMarkerCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(marker) = project.timeline.find_marker_mut(&self.marker_id) {
            marker.beat = self.new_beat;
        }
        project.timeline.sort_markers();
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(marker) = project.timeline.find_marker_mut(&self.marker_id) {
            marker.beat = self.old_beat;
        }
        project.timeline.sort_markers();
    }

    fn description(&self) -> &str {
        "Move Marker"
    }
}

// ── Rename Marker ───────────────────────────────────────────────

#[derive(Debug)]
pub struct RenameMarkerCommand {
    marker_id: MarkerId,
    old_name: String,
    new_name: String,
}

impl RenameMarkerCommand {
    pub fn new(marker_id: MarkerId, old_name: String, new_name: String) -> Self {
        Self {
            marker_id,
            old_name,
            new_name,
        }
    }
}

impl Command for RenameMarkerCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(marker) = project.timeline.find_marker_mut(&self.marker_id) {
            marker.name = self.new_name.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(marker) = project.timeline.find_marker_mut(&self.marker_id) {
            marker.name = self.old_name.clone();
        }
    }

    fn description(&self) -> &str {
        "Rename Marker"
    }
}
