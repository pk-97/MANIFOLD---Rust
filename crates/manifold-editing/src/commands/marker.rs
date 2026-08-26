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

// ── Toggle Section Marker ───────────────────────────────────────

/// Toggle a marker's `is_section_boundary` flag. Shaped like
/// `DeleteMarkerCommand`: execute flips the flag and stores the old value so
/// undo restores it exactly (docs/SECTION_EXPORT_DESIGN.md P1, D3).
#[derive(Debug)]
pub struct ToggleMarkerSectionCommand {
    marker_id: MarkerId,
    old_flag: Option<bool>,
}

impl ToggleMarkerSectionCommand {
    pub fn new(marker_id: MarkerId) -> Self {
        Self {
            marker_id,
            old_flag: None,
        }
    }
}

impl Command for ToggleMarkerSectionCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(marker) = project.timeline.find_marker_mut(&self.marker_id) {
            self.old_flag = Some(marker.is_section_boundary);
            marker.is_section_boundary = !marker.is_section_boundary;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(old) = self.old_flag
            && let Some(marker) = project.timeline.find_marker_mut(&self.marker_id)
        {
            marker.is_section_boundary = old;
        }
    }

    fn description(&self) -> &str {
        "Toggle Section Marker"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::marker::TimelineMarker;

    fn marker_at(beat: f32) -> TimelineMarker {
        TimelineMarker::new(Beats::from_f32(beat))
    }

    #[test]
    fn toggle_flips_flag_and_undo_restores() {
        let mut project = Project::default();
        let m = marker_at(4.0).with_name("Drop");
        let id = m.id.clone();
        project.timeline.add_marker(m);

        let mut cmd = ToggleMarkerSectionCommand::new(id.clone());
        cmd.execute(&mut project);
        assert!(
            project.timeline.find_marker(&id).unwrap().is_section_boundary,
            "execute must set the flag"
        );

        cmd.undo(&mut project);
        assert!(
            !project.timeline.find_marker(&id).unwrap().is_section_boundary,
            "undo must restore the stored false"
        );

        // Toggling again from false → true stores the pre-toggle value each time.
        cmd.execute(&mut project);
        cmd.undo(&mut project);
        assert!(!project.timeline.find_marker(&id).unwrap().is_section_boundary);
    }
}
