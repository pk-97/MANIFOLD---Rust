use crate::command::Command;
use crate::commands::effect_target::{EffectTarget, with_effects_mut};
use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_core::project::Project;
use manifold_core::{EffectGroupId, EffectId};

/// Group effects into a rack group.
/// Matches Unity GroupEffectsCommand: makes effects contiguous in the list
/// starting at the position of the first selected effect, stores original
/// indices for undo to restore exact positions, assigns shared groupId.
#[derive(Debug)]
pub struct GroupEffectsCommand {
    target: EffectTarget,
    /// Indices into the effects list at the time of construction.
    effect_indices: Vec<usize>,
    group: Option<EffectGroup>,
    /// Full editing-time snapshots keep undo exact even when grouping moves a
    /// mask card and updates an existing group's mask reference.
    old_effects: Option<Vec<PresetInstance>>,
    old_groups: Option<Vec<EffectGroup>>,
    applied: bool,
    rejection: Option<&'static str>,
}

impl GroupEffectsCommand {
    pub fn new(target: EffectTarget, effect_indices: Vec<usize>, group_name: String) -> Self {
        Self {
            target,
            effect_indices,
            group: Some(EffectGroup::new(group_name)),
            old_effects: None,
            old_groups: None,
            applied: false,
            rejection: None,
        }
    }
}

impl Command for GroupEffectsCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        let indices = self.effect_indices.clone();
        let Some(mut group) = self.group.clone() else {
            self.rejection = Some("group is unavailable");
            return;
        };

        with_effects_mut(project, &self.target, |effects, groups| {
            if indices.is_empty() {
                self.rejection = Some("at least one effect must be selected");
                return;
            }
            let mut sorted_indices = indices.clone();
            sorted_indices.sort_unstable();
            sorted_indices.dedup();
            if sorted_indices.len() != indices.len()
                || sorted_indices.iter().any(|&idx| idx >= effects.len())
            {
                self.rejection = Some("effect selection is invalid");
                return;
            }

            let selected_ids: Vec<EffectId> = sorted_indices
                .iter()
                .map(|&idx| effects[idx].id.clone())
                .collect();
            let selected_set = |id: &EffectId| selected_ids.iter().any(|selected| selected == id);

            // A masked group may only move as one contiguous unit. This also
            // prevents its mask card from becoming an ordinary colour effect.
            for old_group in groups.iter() {
                let member_indices: Vec<usize> = effects
                    .iter()
                    .enumerate()
                    .filter(|(_, effect)| effect.group_id.as_ref() == Some(&old_group.id))
                    .map(|(idx, _)| idx)
                    .collect();
                if old_group.mask_effect_id.is_some()
                    && member_indices.iter().any(|idx| indices.contains(idx))
                    && member_indices.iter().any(|idx| !indices.contains(idx))
                {
                    self.rejection = Some("masked groups must move as a whole");
                    return;
                }
            }

            let selected_mask = groups.iter().find_map(|old_group| {
                old_group
                    .mask_effect_id
                    .clone()
                    .filter(|mask_id| selected_set(mask_id))
            });
            if groups
                .iter()
                .filter_map(|old_group| old_group.mask_effect_id.as_ref())
                .filter(|mask_id| selected_set(mask_id))
                .count()
                > 1
            {
                self.rejection = Some("a group can have only one mask");
                return;
            }

            self.old_effects = Some(effects.clone());
            self.old_groups = Some(groups.clone());

            if let Some(mask_id) = selected_mask.clone() {
                group.mask_effect_id = Some(mask_id);
                // Keep a nested group at the same parent when regrouping it.
                if let Some(old_group) = groups
                    .iter()
                    .find(|old| old.mask_effect_id.as_ref() == selected_mask.as_ref())
                {
                    group.parent_group_id = old_group.parent_group_id.clone();
                }
                for old_group in groups.iter_mut() {
                    if old_group.mask_effect_id == selected_mask {
                        old_group.mask_effect_id = None;
                    }
                }
            }

            let mut grouped: Vec<PresetInstance> = indices
                .iter()
                .map(|&idx| effects[idx].clone())
                .collect();
            if let Some(mask_id) = selected_mask
                && let Some(mask_pos) = grouped.iter().position(|effect| effect.id == mask_id)
            {
                let mask = grouped.remove(mask_pos);
                grouped.insert(0, mask);
            }

            let insert_at = sorted_indices[0];
            for &idx in sorted_indices.iter().rev() {
                effects.remove(idx);
            }
            let group_id = group.id.clone();
            for (offset, mut effect) in grouped.into_iter().enumerate() {
                effect.group_id = Some(group_id.clone());
                effects.insert((insert_at + offset).min(effects.len()), effect);
            }
            groups.push(group);
            self.applied = true;
        });
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let Some(old_effects) = self.old_effects.clone() else {
            return;
        };
        let Some(old_groups) = self.old_groups.clone() else {
            return;
        };
        with_effects_mut(project, &self.target, |effects, groups| {
            *effects = old_effects;
            *groups = old_groups;
        });
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Group Effects"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

/// Ungroup effects from a rack group.
#[derive(Debug)]
pub struct UngroupEffectsCommand {
    target: EffectTarget,
    group_id: EffectGroupId,
    old_effects: Option<Vec<PresetInstance>>,
    old_groups: Option<Vec<EffectGroup>>,
    applied: bool,
}

impl UngroupEffectsCommand {
    pub fn new(target: EffectTarget, group_id: EffectGroupId) -> Self {
        Self {
            target,
            group_id,
            old_effects: None,
            old_groups: None,
            applied: false,
        }
    }
}

impl Command for UngroupEffectsCommand {
    fn execute(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();

        with_effects_mut(project, &self.target, |effects, groups| {
            let Some(_) = groups.iter().find(|g| g.id == gid) else {
                self.applied = false;
                return;
            };
            self.old_effects = Some(effects.clone());
            self.old_groups = Some(groups.clone());

            let mask_id = groups
                .iter()
                .find(|group| group.id == gid)
                .and_then(|group| group.mask_effect_id.clone());
            if let Some(mask_id) = mask_id {
                effects.retain(|effect| effect.id != mask_id);
            }
            for effect in effects.iter_mut() {
                if effect.group_id.as_deref() == Some(&gid) {
                    effect.group_id = None;
                }
            }
            groups.retain(|g| g.id != gid);
            self.applied = true;
        });
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let Some(old_effects) = self.old_effects.clone() else {
            return;
        };
        let Some(old_groups) = self.old_groups.clone() else {
            return;
        };
        with_effects_mut(project, &self.target, |effects, groups| {
            *effects = old_effects;
            *groups = old_groups;
        });
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Ungroup Effects"
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

/// Add an ordinary effect instance as the mask member of a group.
#[derive(Debug)]
pub struct AddGroupMaskCommand {
    target: EffectTarget,
    effect_indices: Vec<usize>,
    mask: PresetInstance,
    created_group: Option<EffectGroup>,
    old_effects: Option<Vec<PresetInstance>>,
    old_groups: Option<Vec<EffectGroup>>,
    applied: bool,
    rejection: Option<&'static str>,
}

impl AddGroupMaskCommand {
    pub fn new(target: EffectTarget, effect_indices: Vec<usize>, mask: PresetInstance) -> Self {
        Self {
            target,
            effect_indices,
            mask,
            created_group: None,
            old_effects: None,
            old_groups: None,
            applied: false,
            rejection: None,
        }
    }
}

impl Command for AddGroupMaskCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        let indices = self.effect_indices.clone();
        let mask = self.mask.clone();
        with_effects_mut(project, &self.target, |effects, groups| {
            if indices.is_empty() {
                self.rejection = Some("at least one effect must be selected");
                return;
            }
            let mut sorted = indices.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if sorted.len() != indices.len() || sorted.iter().any(|&idx| idx >= effects.len()) {
                self.rejection = Some("effect selection is invalid");
                return;
            }
            if effects.iter().any(|effect| effect.id == mask.id) {
                self.rejection = Some("mask effect already exists");
                return;
            }
            if groups.iter().any(|group| {
                group
                    .mask_effect_id
                    .as_ref()
                    .is_some_and(|mask_id| sorted.iter().any(|&idx| effects[idx].id == *mask_id))
            }) {
                self.rejection = Some("selected effects already have a mask");
                return;
            }
            if groups.iter().any(|group| {
                group.mask_effect_id.is_some()
                    && sorted
                        .iter()
                        .any(|&idx| effects[idx].group_id.as_ref() == Some(&group.id))
            }) {
                self.rejection = Some("selected group already has a mask");
                return;
            }

            let selected_group = sorted
                .iter()
                .map(|&idx| effects[idx].group_id.clone())
                .collect::<Vec<_>>();
            let existing_group_id = selected_group
                .first()
                .cloned()
                .filter(|gid| gid.is_some() && selected_group.iter().all(|id| id == gid))
                .flatten()
                .filter(|gid| groups.iter().any(|group| &group.id == gid));

            self.old_effects = Some(effects.clone());
            self.old_groups = Some(groups.clone());

            let attaches_existing = existing_group_id.is_some();
            let group_id = if let Some(group_id) = existing_group_id {
                group_id
            } else if let Some(group) = &self.created_group {
                let group_id = group.id.clone();
                groups.push(group.clone());
                group_id
            } else {
                let group = EffectGroup::new("Masked Group".to_string());
                let group_id = group.id.clone();
                self.created_group = Some(group.clone());
                groups.push(group);
                group_id
            };

            let member_indices: Vec<usize> = effects
                .iter()
                .enumerate()
                .filter(|(_, effect)| effect.group_id.as_ref() == Some(&group_id))
                .map(|(idx, _)| idx)
                .collect();
            let insert_at = member_indices.first().copied().unwrap_or(sorted[0]);
            if attaches_existing {
                // Existing groups can be malformed after legacy structural
                // edits. Normalize the full group before inserting the mask so
                // the renderer sees one contiguous membership run.
                let members: Vec<PresetInstance> = member_indices
                    .iter()
                    .map(|&idx| effects[idx].clone())
                    .collect();
                for &idx in member_indices.iter().rev() {
                    effects.remove(idx);
                }
                for (offset, member) in members.into_iter().enumerate() {
                    effects.insert(insert_at + offset, member);
                }
            } else {
                for &idx in sorted.iter().rev() {
                    effects.remove(idx);
                }
                for (offset, &old_idx) in sorted.iter().enumerate() {
                    let mut effect = self.old_effects.as_ref().unwrap()[old_idx].clone();
                    effect.group_id = Some(group_id.clone());
                    effects.insert((sorted[0] + offset).min(effects.len()), effect);
                }
            }

            let mut mask = mask;
            mask.group_id = Some(group_id.clone());
            effects.insert(insert_at.min(effects.len()), mask.clone());
            if let Some(group) = groups.iter_mut().find(|group| group.id == group_id) {
                group.mask_effect_id = Some(mask.id);
            }
            self.applied = true;
        });
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let Some(old_effects) = self.old_effects.clone() else {
            return;
        };
        let Some(old_groups) = self.old_groups.clone() else {
            return;
        };
        with_effects_mut(project, &self.target, |effects, groups| {
            *effects = old_effects;
            *groups = old_groups;
        });
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Add Group Mask"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

/// Toggle a group's enabled state.
#[derive(Debug)]
pub struct ToggleGroupCommand {
    target: EffectTarget,
    group_id: EffectGroupId,
    old_enabled: bool,
    new_enabled: bool,
}

impl ToggleGroupCommand {
    pub fn new(
        target: EffectTarget,
        group_id: EffectGroupId,
        old_enabled: bool,
        new_enabled: bool,
    ) -> Self {
        Self {
            target,
            group_id,
            old_enabled,
            new_enabled,
        }
    }
}

impl Command for ToggleGroupCommand {
    fn execute(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();
        let val = self.new_enabled;
        with_effects_mut(project, &self.target, |_effects, groups| {
            if let Some(group) = groups.iter_mut().find(|g| g.id == gid) {
                group.enabled = val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();
        let val = self.old_enabled;
        with_effects_mut(project, &self.target, |_effects, groups| {
            if let Some(group) = groups.iter_mut().find(|g| g.id == gid) {
                group.enabled = val;
            }
        });
    }

    fn description(&self) -> &str {
        "Toggle Group"
    }
}

/// Rename a group.
#[derive(Debug)]
pub struct RenameGroupCommand {
    target: EffectTarget,
    group_id: EffectGroupId,
    old_name: String,
    new_name: String,
}

impl RenameGroupCommand {
    pub fn new(
        target: EffectTarget,
        group_id: EffectGroupId,
        old_name: String,
        new_name: String,
    ) -> Self {
        Self {
            target,
            group_id,
            old_name,
            new_name,
        }
    }
}

impl Command for RenameGroupCommand {
    fn execute(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();
        let name = self.new_name.clone();
        with_effects_mut(project, &self.target, |_effects, groups| {
            if let Some(group) = groups.iter_mut().find(|g| g.id == gid) {
                group.name = name;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();
        let name = self.old_name.clone();
        with_effects_mut(project, &self.target, |_effects, groups| {
            if let Some(group) = groups.iter_mut().find(|g| g.id == gid) {
                group.name = name;
            }
        });
    }

    fn description(&self) -> &str {
        "Rename Group"
    }
}

/// Change group wet/dry mix.
#[derive(Debug)]
pub struct ChangeGroupWetDryCommand {
    target: EffectTarget,
    group_id: EffectGroupId,
    old_wet_dry: f32,
    new_wet_dry: f32,
}

impl ChangeGroupWetDryCommand {
    pub fn new(
        target: EffectTarget,
        group_id: EffectGroupId,
        old_wet_dry: f32,
        new_wet_dry: f32,
    ) -> Self {
        Self {
            target,
            group_id,
            old_wet_dry,
            new_wet_dry,
        }
    }
}

impl Command for ChangeGroupWetDryCommand {
    fn execute(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();
        let val = self.new_wet_dry;
        with_effects_mut(project, &self.target, |_effects, groups| {
            if let Some(group) = groups.iter_mut().find(|g| g.id == gid) {
                group.wet_dry = val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();
        let val = self.old_wet_dry;
        with_effects_mut(project, &self.target, |_effects, groups| {
            if let Some(group) = groups.iter_mut().find(|g| g.id == gid) {
                group.wet_dry = val;
            }
        });
    }

    fn description(&self) -> &str {
        "Change Group Wet/Dry"
    }
}

/// Move an entire rack (all effects with matching groupId) to a new position.
/// Maintains contiguity invariant. Matches Unity ReorderRackCommand.
#[derive(Debug)]
pub struct ReorderRackCommand {
    target: EffectTarget,
    group_id: EffectGroupId,
    target_insert_index: usize,
    /// Original indices of all group members, captured on first execute.
    original_indices: Vec<usize>,
}

impl ReorderRackCommand {
    pub fn new(target: EffectTarget, group_id: EffectGroupId, target_insert_index: usize) -> Self {
        Self {
            target,
            group_id,
            target_insert_index,
            original_indices: Vec::new(),
        }
    }
}

impl Command for ReorderRackCommand {
    fn execute(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();
        let target_idx = self.target_insert_index;

        with_effects_mut(project, &self.target, |effects, _groups| {
            // Snapshot original indices on first execute
            if self.original_indices.is_empty() {
                for (i, e) in effects.iter().enumerate() {
                    if e.group_id.as_deref() == Some(&gid) {
                        self.original_indices.push(i);
                    }
                }
            }

            // Collect members in list order
            let members: Vec<PresetInstance> = self
                .original_indices
                .iter()
                .filter_map(|&i| effects.get(i).cloned())
                .collect();

            // Count how many members were before the target (their removal shifts target down)
            let removed_before = self
                .original_indices
                .iter()
                .filter(|&&i| i < target_idx)
                .count();

            // Remove all members (reverse order to preserve indices)
            for &idx in self.original_indices.iter().rev() {
                if idx < effects.len() {
                    effects.remove(idx);
                }
            }

            // Re-insert contiguously at adjusted target
            let insert_at = target_idx.saturating_sub(removed_before).min(effects.len());
            for (i, member) in members.into_iter().enumerate() {
                effects.insert(insert_at + i, member);
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();
        let original_indices = self.original_indices.clone();

        with_effects_mut(project, &self.target, |effects, _groups| {
            // Collect current members
            let members: Vec<PresetInstance> = effects
                .iter()
                .filter(|e| e.group_id.as_deref() == Some(&gid))
                .cloned()
                .collect();

            // Remove all members
            effects.retain(|e| e.group_id.as_deref() != Some(&gid));

            // Re-insert at original positions (ascending order)
            let mut pairs: Vec<(usize, PresetInstance)> = original_indices
                .iter()
                .zip(members)
                .map(|(&idx, fx)| (idx, fx))
                .collect();
            pairs.sort_by_key(|(idx, _)| *idx);

            for (idx, fx) in pairs {
                let insert_at = idx.min(effects.len());
                effects.insert(insert_at, fx);
            }
        });
    }

    fn description(&self) -> &str {
        "Move Rack"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use crate::commands::effects::{
        AddEffectCommand, RemoveEffectCommand, ReorderEffectCommand,
        ReorderEffectGroupCommand,
    };
    use manifold_core::effects::PresetInstance;
    use manifold_core::{EffectId, PresetTypeId};

    fn effect(name: &str) -> PresetInstance {
        PresetInstance::new(PresetTypeId::from_string(name.to_string()))
    }

    fn master_target() -> EffectTarget {
        EffectTarget::Master
    }

    #[test]
    fn group_mask_add_single_creates_stable_group_on_redo() {
        let mut project = Project::default();
        project.settings.master_effects.push(effect("Colour"));
        let mask = effect("Mask");
        let mask_id = mask.id.clone();
        let mut command = AddGroupMaskCommand::new(master_target(), vec![0], mask);

        command.execute(&mut project);
        let group = project.settings.master_effect_groups.as_ref().unwrap()[0].clone();
        assert_eq!(project.settings.master_effects.len(), 2);
        assert_eq!(project.settings.master_effects[0].id, mask_id);
        assert_eq!(group.mask_effect_id, Some(mask_id.clone()));
        let group_id = group.id.clone();

        command.undo(&mut project);
        assert!(project.settings.master_effect_groups.as_ref().unwrap().is_empty());
        command.execute(&mut project);
        let redo_group = project.settings.master_effect_groups.as_ref().unwrap()[0].clone();
        assert_eq!(redo_group.id, group_id);
        assert_eq!(redo_group.mask_effect_id, Some(mask_id));
    }

    #[test]
    fn group_mask_add_existing_group_compacts_members_and_undoes_exactly() {
        let mut project = Project::default();
        let group = EffectGroup::new("Existing".into());
        let gid = group.id.clone();
        let mut first = effect("First");
        first.group_id = Some(gid.clone());
        let middle = effect("Middle");
        let mut last = effect("Last");
        last.group_id = Some(gid.clone());
        project.settings.master_effects = vec![first, middle, last];
        project.settings.master_effect_groups = Some(vec![group]);
        let original_ids: Vec<EffectId> = project
            .settings
            .master_effects
            .iter()
            .map(|effect| effect.id.clone())
            .collect();
        let mask = effect("Mask");
        let mask_id = mask.id.clone();
        let mut command = AddGroupMaskCommand::new(master_target(), vec![0], mask);

        command.execute(&mut project);
        let effects = &project.settings.master_effects;
        let members: Vec<usize> = effects
            .iter()
            .enumerate()
            .filter(|(_, effect)| effect.group_id.as_ref() == Some(&gid))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(members, vec![0, 1, 2]);
        assert_eq!(effects[0].id, mask_id);
        assert_eq!(project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id, Some(mask_id.clone()));

        command.undo(&mut project);
        assert_eq!(
            project.settings.master_effects.iter().map(|effect| effect.id.clone()).collect::<Vec<_>>(),
            original_ids
        );
        assert_eq!(project.settings.master_effects[0].group_id, Some(gid));
        assert_eq!(project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id, None);
    }

    #[test]
    fn group_mask_remove_and_undo_redo_restores_reference() {
        let mut project = Project::default();
        project.settings.master_effects.push(effect("Colour"));
        let mask = effect("Mask");
        let mask_id = mask.id.clone();
        let mut add = AddGroupMaskCommand::new(master_target(), vec![0], mask);
        add.execute(&mut project);
        let group_id = project.settings.master_effect_groups.as_ref().unwrap()[0].id.clone();
        let mask_index = project.settings.master_effects.iter().position(|effect| effect.id == mask_id).unwrap();
        let removed = project.settings.master_effects[mask_index].clone();
        let mut remove = RemoveEffectCommand::new(master_target(), removed, mask_index);

        remove.execute(&mut project);
        assert_eq!(project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id, None);
        remove.undo(&mut project);
        assert_eq!(project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id, Some(mask_id.clone()));
        assert_eq!(project.settings.master_effect_groups.as_ref().unwrap()[0].id, group_id);
        remove.execute(&mut project);
        assert_eq!(project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id, None);
    }

    #[test]
    fn group_mask_ungroup_removes_mask_and_undo_redo_restores_exact_membership() {
        let mut project = Project::default();
        project.settings.master_effects.push(effect("Colour"));
        let mask = effect("Mask");
        let mask_id = mask.id.clone();
        let mut add = AddGroupMaskCommand::new(master_target(), vec![0], mask);
        add.execute(&mut project);
        let group = project.settings.master_effect_groups.as_ref().unwrap()[0].clone();
        let gid = group.id.clone();
        let original_ids: Vec<EffectId> = project.settings.master_effects.iter().map(|effect| effect.id.clone()).collect();
        let mut ungroup = UngroupEffectsCommand::new(master_target(), gid.clone());

        ungroup.execute(&mut project);
        assert!(project.settings.master_effects.iter().all(|effect| effect.id != mask_id));
        assert!(project.settings.master_effect_groups.as_ref().unwrap().is_empty());
        ungroup.undo(&mut project);
        assert_eq!(project.settings.master_effects.iter().map(|effect| effect.id.clone()).collect::<Vec<_>>(), original_ids);
        assert_eq!(project.settings.master_effect_groups.as_ref().unwrap()[0].id, gid);
        assert_eq!(project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id, Some(mask_id.clone()));
        ungroup.execute(&mut project);
        assert!(project.settings.master_effects.iter().all(|effect| effect.id != mask_id));
    }

    #[test]
    fn group_mask_structural_commands_reject_split_membership() {
        let mut project = Project::default();
        project.settings.master_effects.push(effect("Colour"));
        let mask = effect("Mask");
        let mut add = AddGroupMaskCommand::new(master_target(), vec![0], mask);
        add.execute(&mut project);
        let gid = project.settings.master_effect_groups.as_ref().unwrap()[0].id.clone();

        let mut insert = AddEffectCommand::new(master_target(), effect("Inserted"), 1);
        insert.execute(&mut project);
        assert!(insert.rejection_reason().is_some());

        project.settings.master_effects.push(effect("Outside"));
        let mut move_into = ReorderEffectCommand::new(master_target(), 2, 1);
        move_into.execute(&mut project);
        assert!(move_into.rejection_reason().is_some());
        assert!(project.settings.master_effects.iter().any(|effect| effect.group_id.as_ref() == Some(&gid)));

        let old_effects = project.settings.master_effects.clone();
        let mut broken = old_effects.clone();
        let outside = broken.pop().unwrap();
        broken.insert(1, outside);
        let mut snapshot = ReorderEffectGroupCommand::new(master_target(), old_effects, broken);
        snapshot.execute(&mut project);
        assert!(snapshot.rejection_reason().is_some());
    }
}
