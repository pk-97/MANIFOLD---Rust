//! Undoable commands for the project's [`StageLayout`] — the physical
//! multi-display / totem arrangement. Edits route through `EditingService`
//! like every other project mutation. See `docs/MULTI_DISPLAY_DESIGN.md`.
//!
//! Placements are addressed by [`OutputId`] (stable identity), not index, so
//! a command stays correct even if the placement list is reordered between
//! capture and apply — same doctrine as `AudioSendId` addressing in
//! `commands/audio_setup.rs`. `AddDisplayPlacementCommand` is the exception —
//! it carries the whole placement (id minted at construction) so
//! execute/undo are deterministic.
//!
//! Derivation (`derive_stage`) is not run by these commands: it is a pure,
//! cheap function callers re-run after any mutation (§5 "Mutations"), not
//! state these commands need to maintain.

use crate::command::Command;
use manifold_core::project::Project;
use manifold_core::stage::{DisplayIdentity, DisplayPlacement, OutputAdvanced, OutputId, Rotation};

/// Add a placement. The placement (with its minted id) is supplied by the caller.
#[derive(Debug)]
pub struct AddDisplayPlacementCommand {
    placement: DisplayPlacement,
}

impl AddDisplayPlacementCommand {
    pub fn new(placement: DisplayPlacement) -> Self {
        Self { placement }
    }
}

impl Command for AddDisplayPlacementCommand {
    fn execute(&mut self, project: &mut Project) {
        project
            .settings
            .stage_layout
            .placements
            .push(self.placement.clone());
    }

    fn undo(&mut self, project: &mut Project) {
        let id = self.placement.id;
        project
            .settings
            .stage_layout
            .placements
            .retain(|p| p.id != id);
    }

    fn description(&self) -> &str {
        "Add Display Placement"
    }
}

/// Remove a placement by id. Captures it and its position on execute so undo
/// restores it at the same index.
#[derive(Debug)]
pub struct RemoveDisplayPlacementCommand {
    id: OutputId,
    removed: Option<(usize, DisplayPlacement)>,
}

impl RemoveDisplayPlacementCommand {
    pub fn new(id: OutputId) -> Self {
        Self { id, removed: None }
    }
}

impl Command for RemoveDisplayPlacementCommand {
    fn execute(&mut self, project: &mut Project) {
        let placements = &mut project.settings.stage_layout.placements;
        if let Some(pos) = placements.iter().position(|p| p.id == self.id) {
            let removed = placements.remove(pos);
            self.removed = Some((pos, removed));
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((pos, placement)) = self.removed.take() {
            let placements = &mut project.settings.stage_layout.placements;
            let at = pos.min(placements.len());
            placements.insert(at, placement);
        }
    }

    fn description(&self) -> &str {
        "Remove Display Placement"
    }
}

/// Rename a placement.
#[derive(Debug)]
pub struct RenameDisplayPlacementCommand {
    id: OutputId,
    old_name: String,
    new_name: String,
}

impl RenameDisplayPlacementCommand {
    pub fn new(id: OutputId, old_name: String, new_name: String) -> Self {
        Self {
            id,
            old_name,
            new_name,
        }
    }
}

impl Command for RenameDisplayPlacementCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.name = self.new_name.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.name = self.old_name.clone();
        }
    }

    fn description(&self) -> &str {
        "Rename Display Placement"
    }
}

/// Move/rotate a placement on the stage plan — one undo step per drag or
/// numeric-entry commit (position and rotation change together, §5).
#[derive(Debug)]
pub struct SetDisplayPlacementTransformCommand {
    id: OutputId,
    old_position_mm: [f32; 2],
    old_rotation: Rotation,
    new_position_mm: [f32; 2],
    new_rotation: Rotation,
}

impl SetDisplayPlacementTransformCommand {
    pub fn new(
        id: OutputId,
        old_position_mm: [f32; 2],
        old_rotation: Rotation,
        new_position_mm: [f32; 2],
        new_rotation: Rotation,
    ) -> Self {
        Self {
            id,
            old_position_mm,
            old_rotation,
            new_position_mm,
            new_rotation,
        }
    }
}

impl Command for SetDisplayPlacementTransformCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.position_mm = self.new_position_mm;
            p.rotation = self.new_rotation;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.position_mm = self.old_position_mm;
            p.rotation = self.old_rotation;
        }
    }

    fn description(&self) -> &str {
        "Move Display Placement"
    }
}

/// Correct a placement's physical panel size (EDID prefill is often right,
/// sometimes garbage — always editable, §5).
#[derive(Debug)]
pub struct SetDisplayPhysicalSizeCommand {
    id: OutputId,
    old_size_mm: [f32; 2],
    new_size_mm: [f32; 2],
}

impl SetDisplayPhysicalSizeCommand {
    pub fn new(id: OutputId, old_size_mm: [f32; 2], new_size_mm: [f32; 2]) -> Self {
        Self {
            id,
            old_size_mm,
            new_size_mm,
        }
    }
}

impl Command for SetDisplayPhysicalSizeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.physical_size_mm = self.new_size_mm;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.physical_size_mm = self.old_size_mm;
        }
    }

    fn description(&self) -> &str {
        "Set Display Physical Size"
    }
}

/// Correct a placement's native pixel resolution (the mode MANIFOLD drives it at).
#[derive(Debug)]
pub struct SetDisplayNativeResolutionCommand {
    id: OutputId,
    old_resolution: [u32; 2],
    new_resolution: [u32; 2],
}

impl SetDisplayNativeResolutionCommand {
    pub fn new(id: OutputId, old_resolution: [u32; 2], new_resolution: [u32; 2]) -> Self {
        Self {
            id,
            old_resolution,
            new_resolution,
        }
    }
}

impl Command for SetDisplayNativeResolutionCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.native_resolution = self.new_resolution;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.native_resolution = self.old_resolution;
        }
    }

    fn description(&self) -> &str {
        "Set Display Native Resolution"
    }
}

/// Enable/disable a placement without deleting it (excluded from
/// `derive_stage` entirely while disabled).
#[derive(Debug)]
pub struct SetDisplayEnabledCommand {
    id: OutputId,
    old_enabled: bool,
    new_enabled: bool,
}

impl SetDisplayEnabledCommand {
    pub fn new(id: OutputId, old_enabled: bool, new_enabled: bool) -> Self {
        Self {
            id,
            old_enabled,
            new_enabled,
        }
    }
}

impl Command for SetDisplayEnabledCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.enabled = self.new_enabled;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.enabled = self.old_enabled;
        }
    }

    fn description(&self) -> &str {
        "Toggle Display Enabled"
    }
}

/// Assign or clear a placement's live-display identity (the "assign" picker,
/// §5). `None` = unassigned — the placement still renders but presents nowhere.
#[derive(Debug)]
pub struct SetDisplayIdentityCommand {
    id: OutputId,
    old_identity: Option<DisplayIdentity>,
    new_identity: Option<DisplayIdentity>,
}

impl SetDisplayIdentityCommand {
    pub fn new(
        id: OutputId,
        old_identity: Option<DisplayIdentity>,
        new_identity: Option<DisplayIdentity>,
    ) -> Self {
        Self {
            id,
            old_identity,
            new_identity,
        }
    }
}

impl Command for SetDisplayIdentityCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.identity = self.new_identity.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.identity = self.old_identity.clone();
        }
    }

    fn description(&self) -> &str {
        "Assign Display Identity"
    }
}

/// Replace a placement's whole advanced-flap config (keystone, color trim,
/// tonemap override, density cap) — one undo step per flap edit, matching
/// `SetAudioSendAnalysisCommand`'s whole-config-at-once shape.
#[derive(Debug)]
pub struct SetDisplayAdvancedCommand {
    id: OutputId,
    old: OutputAdvanced,
    new: OutputAdvanced,
}

impl SetDisplayAdvancedCommand {
    pub fn new(id: OutputId, old: OutputAdvanced, new: OutputAdvanced) -> Self {
        Self { id, old, new }
    }
}

impl Command for SetDisplayAdvancedCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.advanced = self.new.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(p) = project.settings.stage_layout.find_mut(self.id) {
            p.advanced = self.old.clone();
        }
    }

    fn description(&self) -> &str {
        "Set Display Advanced Settings"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_placement(id: u64) -> DisplayPlacement {
        DisplayPlacement {
            id: OutputId(id),
            name: format!("Totem {id}"),
            physical_size_mm: [500.0, 1000.0],
            native_resolution: [1080, 1920],
            position_mm: [0.0, 0.0],
            rotation: Rotation::R0,
            identity: None,
            enabled: true,
            advanced: OutputAdvanced::default(),
        }
    }

    #[test]
    fn add_then_undo_removes_the_placement() {
        let mut project = Project::default();
        let placement = sample_placement(0);
        let id = placement.id;
        let mut cmd = AddDisplayPlacementCommand::new(placement);

        cmd.execute(&mut project);
        assert!(project.settings.stage_layout.find(id).is_some());
        cmd.undo(&mut project);
        assert!(project.settings.stage_layout.find(id).is_none());
    }

    #[test]
    fn remove_undo_restores_at_same_index() {
        let mut project = Project::default();
        let a = sample_placement(0);
        let b = sample_placement(1);
        let c = sample_placement(2);
        let b_id = b.id;
        project.settings.stage_layout.placements = vec![a, b, c];

        let mut cmd = RemoveDisplayPlacementCommand::new(b_id);
        cmd.execute(&mut project);
        assert_eq!(project.settings.stage_layout.placements.len(), 2);
        assert!(project.settings.stage_layout.find(b_id).is_none());

        cmd.undo(&mut project);
        assert_eq!(project.settings.stage_layout.placements[1].id, b_id);
    }

    #[test]
    fn rename_round_trips() {
        let mut project = Project::default();
        let placement = sample_placement(0);
        let id = placement.id;
        project.settings.stage_layout.placements.push(placement);

        let mut cmd =
            RenameDisplayPlacementCommand::new(id, "Totem 0".into(), "Totem L".into());
        cmd.execute(&mut project);
        assert_eq!(project.settings.stage_layout.find(id).unwrap().name, "Totem L");
        cmd.undo(&mut project);
        assert_eq!(project.settings.stage_layout.find(id).unwrap().name, "Totem 0");
    }

    #[test]
    fn transform_round_trips() {
        let mut project = Project::default();
        let placement = sample_placement(0);
        let id = placement.id;
        project.settings.stage_layout.placements.push(placement);

        let mut cmd = SetDisplayPlacementTransformCommand::new(
            id,
            [0.0, 0.0],
            Rotation::R0,
            [3500.0, 0.0],
            Rotation::R90,
        );
        cmd.execute(&mut project);
        let p = project.settings.stage_layout.find(id).unwrap();
        assert_eq!(p.position_mm, [3500.0, 0.0]);
        assert_eq!(p.rotation, Rotation::R90);

        cmd.undo(&mut project);
        let p = project.settings.stage_layout.find(id).unwrap();
        assert_eq!(p.position_mm, [0.0, 0.0]);
        assert_eq!(p.rotation, Rotation::R0);
    }

    #[test]
    fn physical_size_round_trips() {
        let mut project = Project::default();
        let placement = sample_placement(0);
        let id = placement.id;
        project.settings.stage_layout.placements.push(placement);

        let mut cmd =
            SetDisplayPhysicalSizeCommand::new(id, [500.0, 1000.0], [540.0, 960.0]);
        cmd.execute(&mut project);
        assert_eq!(
            project.settings.stage_layout.find(id).unwrap().physical_size_mm,
            [540.0, 960.0]
        );
        cmd.undo(&mut project);
        assert_eq!(
            project.settings.stage_layout.find(id).unwrap().physical_size_mm,
            [500.0, 1000.0]
        );
    }

    #[test]
    fn native_resolution_round_trips() {
        let mut project = Project::default();
        let placement = sample_placement(0);
        let id = placement.id;
        project.settings.stage_layout.placements.push(placement);

        let mut cmd =
            SetDisplayNativeResolutionCommand::new(id, [1080, 1920], [2160, 3840]);
        cmd.execute(&mut project);
        assert_eq!(
            project.settings.stage_layout.find(id).unwrap().native_resolution,
            [2160, 3840]
        );
        cmd.undo(&mut project);
        assert_eq!(
            project.settings.stage_layout.find(id).unwrap().native_resolution,
            [1080, 1920]
        );
    }

    #[test]
    fn enabled_toggle_round_trips() {
        let mut project = Project::default();
        let placement = sample_placement(0);
        let id = placement.id;
        project.settings.stage_layout.placements.push(placement);

        let mut cmd = SetDisplayEnabledCommand::new(id, true, false);
        cmd.execute(&mut project);
        assert!(!project.settings.stage_layout.find(id).unwrap().enabled);
        cmd.undo(&mut project);
        assert!(project.settings.stage_layout.find(id).unwrap().enabled);
    }

    #[test]
    fn identity_assign_and_clear_round_trip() {
        let mut project = Project::default();
        let placement = sample_placement(0);
        let id = placement.id;
        project.settings.stage_layout.placements.push(placement);

        let identity = DisplayIdentity {
            uuid: Some("ABC-123".into()),
            name: "Totem L".into(),
        };
        let mut cmd = SetDisplayIdentityCommand::new(id, None, Some(identity.clone()));
        cmd.execute(&mut project);
        assert_eq!(
            project.settings.stage_layout.find(id).unwrap().identity,
            Some(identity)
        );
        cmd.undo(&mut project);
        assert_eq!(project.settings.stage_layout.find(id).unwrap().identity, None);
    }

    #[test]
    fn advanced_settings_round_trip() {
        let mut project = Project::default();
        let placement = sample_placement(0);
        let id = placement.id;
        project.settings.stage_layout.placements.push(placement);

        let old = OutputAdvanced::default();
        let mut new = OutputAdvanced::default();
        new.density_cap_px_per_mm = Some(2.0);

        let mut cmd = SetDisplayAdvancedCommand::new(id, old.clone(), new.clone());
        cmd.execute(&mut project);
        assert_eq!(
            project.settings.stage_layout.find(id).unwrap().advanced.density_cap_px_per_mm,
            Some(2.0)
        );
        cmd.undo(&mut project);
        assert_eq!(
            project.settings.stage_layout.find(id).unwrap().advanced.density_cap_px_per_mm,
            None
        );
    }
}
