use crate::command::Command;
use crate::commands::effect_target::{EffectTarget, with_effects_mut};
use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_core::project::Project;
use manifold_core::{EffectGroupId, EffectId};
use std::collections::HashSet;

fn group_members_contiguous(effects: &[PresetInstance], group_id: &EffectGroupId) -> bool {
    let mut seen_member = false;
    let mut ended = false;
    for effect in effects {
        if effect.group_id.as_ref() == Some(group_id) {
            if ended {
                return false;
            }
            seen_member = true;
        } else if seen_member {
            ended = true;
        }
    }
    true
}

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

            let mut grouped: Vec<PresetInstance> =
                indices.iter().map(|&idx| effects[idx].clone()).collect();
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
    group_id: Option<EffectGroupId>,
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
            group_id: None,
            mask,
            created_group: None,
            old_effects: None,
            old_groups: None,
            applied: false,
            rejection: None,
        }
    }

    /// Capture a group address at menu-open time; resolve its members on the
    /// content thread so intervening selection/reorder changes cannot retarget it.
    pub fn for_group(target: EffectTarget, group_id: EffectGroupId, mask: PresetInstance) -> Self {
        let mut command = Self::new(target, Vec::new(), mask);
        command.group_id = Some(group_id);
        command
    }
}

impl Command for AddGroupMaskCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        let mut indices = self.effect_indices.clone();
        let mask = self.mask.clone();
        with_effects_mut(project, &self.target, |effects, groups| {
            if let Some(group_id) = &self.group_id {
                if !groups.iter().any(|group| &group.id == group_id) {
                    self.rejection = Some("modifier group no longer exists");
                    return;
                }
                indices = effects
                    .iter()
                    .enumerate()
                    .filter(|(_, effect)| effect.group_id.as_ref() == Some(group_id))
                    .map(|(index, _)| index)
                    .collect();
            }
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
            let requested_group_id = self.group_id.as_ref();
            if groups.iter().any(|group| {
                group.mask_effect_id.as_ref().is_some_and(|mask_id| {
                    sorted.iter().any(|&idx| effects[idx].id == *mask_id)
                        && requested_group_id != Some(&group.id)
                })
            }) {
                self.rejection = Some("selected effects already have a mask");
                return;
            }
            if groups.iter().any(|group| {
                group.mask_effect_id.is_some()
                    && requested_group_id != Some(&group.id)
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
                let group = EffectGroup::new("Modifier Group".to_string());
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
                    .filter(|&&idx| {
                        groups
                            .iter()
                            .find(|group| group.id == group_id)
                            .and_then(|group| group.mask_effect_id.as_ref())
                            != Some(&effects[idx].id)
                    })
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

/// Remove the mask member from a group while preserving the group and every
/// ordinary effect in it. Full list snapshots make undo restore graph,
/// parameter and automation state exactly.
#[derive(Debug)]
pub struct RemoveGroupMaskCommand {
    target: EffectTarget,
    group_id: EffectGroupId,
    old_effects: Option<Vec<PresetInstance>>,
    old_groups: Option<Vec<EffectGroup>>,
    applied: bool,
    rejection: Option<&'static str>,
}

impl RemoveGroupMaskCommand {
    pub fn new(target: EffectTarget, group_id: EffectGroupId) -> Self {
        Self {
            target,
            group_id,
            old_effects: None,
            old_groups: None,
            applied: false,
            rejection: None,
        }
    }
}

impl Command for RemoveGroupMaskCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        let group_id = self.group_id.clone();
        with_effects_mut(project, &self.target, |effects, groups| {
            let Some(group) = groups.iter().find(|group| group.id == group_id) else {
                self.rejection = Some("modifier group no longer exists");
                return;
            };
            let Some(mask_id) = group.mask_effect_id.clone() else {
                self.rejection = Some("modifier group has no mask");
                return;
            };
            let Some(mask) = effects.iter().find(|effect| effect.id == mask_id) else {
                self.rejection = Some("group mask effect no longer exists");
                return;
            };
            if mask.group_id.as_ref() != Some(&group_id) {
                self.rejection = Some("group mask effect belongs to another group");
                return;
            }

            self.old_effects = Some(effects.clone());
            self.old_groups = Some(groups.clone());
            effects.retain(|effect| effect.id != mask_id);
            if let Some(group) = groups.iter_mut().find(|group| group.id == group_id) {
                group.mask_effect_id = None;
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
        "Remove Group Mask"
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

/// Set a group's collapsed state, with exact undo/redo behavior.
#[derive(Debug)]
pub struct SetGroupCollapsedCommand {
    target: EffectTarget,
    group_id: EffectGroupId,
    collapsed: bool,
    old_collapsed: Option<bool>,
    applied: bool,
    rejection: Option<&'static str>,
}

impl SetGroupCollapsedCommand {
    pub fn new(target: EffectTarget, group_id: EffectGroupId, collapsed: bool) -> Self {
        Self {
            target,
            group_id,
            collapsed,
            old_collapsed: None,
            applied: false,
            rejection: None,
        }
    }
}

impl Command for SetGroupCollapsedCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        self.old_collapsed = None;
        let group_id = self.group_id.clone();
        let collapsed = self.collapsed;
        let target_exists = with_effects_mut(project, &self.target, |_effects, groups| {
            let Some(group) = groups.iter_mut().find(|group| group.id == group_id) else {
                self.rejection = Some("modifier group no longer exists");
                return;
            };
            if group.collapsed == collapsed {
                return;
            }
            self.old_collapsed = Some(group.collapsed);
            group.collapsed = collapsed;
            self.applied = true;
        });
        if target_exists.is_none() {
            self.rejection = Some("effect target no longer exists");
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let group_id = self.group_id.clone();
        let Some(collapsed) = self.old_collapsed else {
            return;
        };
        with_effects_mut(project, &self.target, |_effects, groups| {
            if let Some(group) = groups.iter_mut().find(|group| group.id == group_id) {
                group.collapsed = collapsed;
            }
        });
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Set Group Collapsed"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }

    fn was_applied(&self) -> bool {
        self.applied
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

/// Move effects by stable ID, optionally changing their group membership.
/// `preserve_groups` is used for whole-group moves and keeps every selected
/// effect's existing membership intact.
#[derive(Debug)]
pub struct MoveEffectsToGroupCommand {
    target: EffectTarget,
    ids: Vec<EffectId>,
    before: Option<EffectId>,
    destination_group: Option<EffectGroupId>,
    preserve_groups: bool,
    old_effects: Option<Vec<PresetInstance>>,
    old_groups: Option<Vec<EffectGroup>>,
    applied: bool,
    rejection: Option<&'static str>,
}

impl MoveEffectsToGroupCommand {
    pub fn new(
        target: EffectTarget,
        ids: Vec<EffectId>,
        before: Option<EffectId>,
        destination_group: Option<EffectGroupId>,
        preserve_groups: bool,
    ) -> Self {
        Self {
            target,
            ids,
            before,
            destination_group,
            preserve_groups,
            old_effects: None,
            old_groups: None,
            applied: false,
            rejection: None,
        }
    }
}

impl Command for MoveEffectsToGroupCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        let ids = self.ids.clone();
        let before = self.before.clone();
        let destination_group = self.destination_group.clone();
        let preserve_groups = self.preserve_groups;

        let target_exists = with_effects_mut(project, &self.target, |effects, groups| {
            if ids.is_empty() {
                self.rejection = Some("at least one effect must be selected");
                return;
            }
            let selected_ids: HashSet<EffectId> = ids.iter().cloned().collect();
            if selected_ids.len() != ids.len() {
                self.rejection = Some("effect selection contains duplicate IDs");
                return;
            }
            if selected_ids
                .iter()
                .any(|id| !effects.iter().any(|effect| &effect.id == id))
            {
                self.rejection = Some("effect selection contains a stale ID");
                return;
            }
            if let Some(before) = &before
                && !effects.iter().any(|effect| &effect.id == before)
            {
                self.rejection = Some("move destination is stale");
                return;
            }
            if let Some(group_id) = &destination_group
                && !groups.iter().any(|group| &group.id == group_id)
            {
                self.rejection = Some("destination group no longer exists");
                return;
            }

            let selected_positions: Vec<usize> = effects
                .iter()
                .enumerate()
                .filter_map(|(index, effect)| selected_ids.contains(&effect.id).then_some(index))
                .collect();
            let selected_group_ids: HashSet<EffectGroupId> = selected_positions
                .iter()
                .filter_map(|&index| effects[index].group_id.clone())
                .collect();

            for group_id in &selected_group_ids {
                let members: Vec<usize> = effects
                    .iter()
                    .enumerate()
                    .filter_map(|(index, effect)| {
                        (effect.group_id.as_ref() == Some(group_id)).then_some(index)
                    })
                    .collect();
                let selected_members = members
                    .iter()
                    .filter(|index| selected_ids.contains(&effects[**index].id))
                    .count();
                let group = groups.iter().find(|group| &group.id == group_id);
                if !group_members_contiguous(effects, group_id) {
                    self.rejection = Some("group members are noncontiguous");
                    return;
                }
                if group.is_some_and(|group| group.mask_effect_id.is_some())
                    && selected_members != members.len()
                    && destination_group.as_ref() != Some(group_id)
                {
                    self.rejection = Some("masked groups must move as a whole");
                    return;
                }
                if preserve_groups && selected_members != members.len() {
                    self.rejection = Some("whole-group moves require complete groups");
                    return;
                }
            }

            if preserve_groups {
                if destination_group.is_some() {
                    self.rejection = Some("whole groups cannot be moved into another group");
                    return;
                }
            } else if selected_group_ids.iter().any(|group_id| {
                groups
                    .iter()
                    .find(|group| &group.id == group_id)
                    .is_some_and(|group| group.mask_effect_id.is_some())
                    && destination_group.as_ref() != Some(group_id)
            }) {
                self.rejection = Some("masked groups require a whole-group move");
                return;
            }

            if let Some(before) = &before
                && selected_ids.contains(before)
            {
                return;
            }

            let mut candidate: Vec<PresetInstance> = effects
                .iter()
                .filter(|effect| !selected_ids.contains(&effect.id))
                .cloned()
                .collect();
            let mut moved: Vec<PresetInstance> = effects
                .iter()
                .filter(|effect| selected_ids.contains(&effect.id))
                .cloned()
                .collect();
            if !preserve_groups {
                for effect in &mut moved {
                    effect.group_id = destination_group.clone();
                }
            }
            let insert_at = before
                .as_ref()
                .and_then(|before| candidate.iter().position(|effect| &effect.id == before))
                .unwrap_or(candidate.len());
            candidate.splice(insert_at..insert_at, moved);

            if !groups
                .iter()
                .all(|group| group_members_contiguous(&candidate, &group.id))
            {
                self.rejection = Some("move would split a group");
                return;
            }
            if candidate.len() == effects.len()
                && candidate
                    .iter()
                    .zip(effects.iter())
                    .all(|(candidate, current)| {
                        candidate.id == current.id && candidate.group_id == current.group_id
                    })
            {
                return;
            }

            self.old_effects = Some(effects.clone());
            self.old_groups = Some(groups.clone());
            *effects = candidate;
            self.applied = true;
        });
        if target_exists.is_none() {
            self.rejection = Some("effect target no longer exists");
        }
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
        "Move Effects"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

/// Paste a prepared effect/group payload as one atomic, undoable edit.
#[derive(Debug)]
pub struct PasteEffectsCommand {
    target: EffectTarget,
    effects: Vec<PresetInstance>,
    groups: Vec<EffectGroup>,
    before: Option<EffectId>,
    destination_group: Option<EffectGroupId>,
    old_effects: Option<Vec<PresetInstance>>,
    old_groups: Option<Vec<EffectGroup>>,
    applied: bool,
    rejection: Option<&'static str>,
}

impl PasteEffectsCommand {
    pub fn new(
        target: EffectTarget,
        effects: Vec<PresetInstance>,
        groups: Vec<EffectGroup>,
        before: Option<EffectId>,
        destination_group: Option<EffectGroupId>,
    ) -> Self {
        Self {
            target,
            effects,
            groups,
            before,
            destination_group,
            old_effects: None,
            old_groups: None,
            applied: false,
            rejection: None,
        }
    }
}

impl Command for PasteEffectsCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        let payload_effects = self.effects.clone();
        let payload_groups = self.groups.clone();
        let before = self.before.clone();
        let destination_group = self.destination_group.clone();

        let target_exists = with_effects_mut(project, &self.target, |effects, groups| {
            if payload_effects.is_empty() {
                self.rejection = Some("paste payload is empty");
                return;
            }
            let effect_ids: HashSet<EffectId> = payload_effects
                .iter()
                .map(|effect| effect.id.clone())
                .collect();
            let group_ids: HashSet<EffectGroupId> = payload_groups
                .iter()
                .map(|group| group.id.clone())
                .collect();
            if effect_ids.len() != payload_effects.len() || group_ids.len() != payload_groups.len()
            {
                self.rejection = Some("paste payload contains duplicate IDs");
                return;
            }
            if effect_ids
                .iter()
                .any(|id| effects.iter().any(|effect| &effect.id == id))
                || group_ids
                    .iter()
                    .any(|id| groups.iter().any(|group| &group.id == id))
            {
                self.rejection = Some("paste payload reuses an existing ID");
                return;
            }
            if let Some(before) = &before
                && !effects.iter().any(|effect| &effect.id == before)
            {
                self.rejection = Some("paste destination is stale");
                return;
            }
            if let Some(group_id) = &destination_group
                && !groups.iter().any(|group| &group.id == group_id)
            {
                self.rejection = Some("destination group no longer exists");
                return;
            }
            if destination_group.is_some() && !payload_groups.is_empty() {
                self.rejection = Some("complete groups cannot be pasted into another group");
                return;
            }

            let mut pasted = payload_effects.clone();
            for effect in &mut pasted {
                if let Some(group_id) = &effect.group_id {
                    if !group_ids.contains(group_id) {
                        self.rejection = Some("paste payload has a dangling group ID");
                        return;
                    }
                } else if destination_group.is_some() {
                    effect.group_id = destination_group.clone();
                }
            }
            for group in &payload_groups {
                if let Some(parent) = &group.parent_group_id
                    && !group_ids.contains(parent)
                {
                    self.rejection = Some("paste payload has a dangling parent group ID");
                    return;
                }
                let members: Vec<&PresetInstance> = pasted
                    .iter()
                    .filter(|effect| effect.group_id.as_ref() == Some(&group.id))
                    .collect();
                if members.is_empty() {
                    self.rejection = Some("paste payload contains an empty group");
                    return;
                }
                if let Some(mask_id) = &group.mask_effect_id
                    && !members.iter().any(|effect| &effect.id == mask_id)
                {
                    self.rejection = Some("paste payload has a dangling mask ID");
                    return;
                }
            }

            let mut candidate = effects.clone();
            let insert_at = before
                .as_ref()
                .and_then(|id| candidate.iter().position(|effect| &effect.id == id))
                .unwrap_or(candidate.len());
            candidate.splice(insert_at..insert_at, pasted);
            if !groups
                .iter()
                .all(|group| group_members_contiguous(&candidate, &group.id))
                || !payload_groups
                    .iter()
                    .all(|group| group_members_contiguous(&candidate, &group.id))
            {
                self.rejection = Some("paste would split a group");
                return;
            }

            self.old_effects = Some(effects.clone());
            self.old_groups = Some(groups.clone());
            *effects = candidate;
            groups.extend(payload_groups.clone());
            self.applied = true;
        });
        if target_exists.is_none() {
            self.rejection = Some("effect target no longer exists");
        }
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
        "Paste Effects"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use crate::commands::effects::{
        AddEffectCommand, RemoveEffectCommand, ReorderEffectCommand, ReorderEffectGroupCommand,
    };
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::effects::PresetInstance;
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::{EffectId, PresetTypeId};

    fn effect(name: &str) -> PresetInstance {
        PresetInstance::new(PresetTypeId::from_string(name.to_string()))
    }

    fn mask_with_state(name: &str, amount: f32) -> PresetInstance {
        let mut mask = effect(name);
        mask.params = ParamManifest::from_params(vec![Param::bundled(ParamSpecDef {
            id: "amount".to_string(),
            name: "Amount".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 1.0,
            ..ParamSpecDef::default()
        })]);
        mask.base_tracked = true;
        assert!(mask.set_base_param("amount", amount));
        mask.graph = Some(manifold_core::effect_graph_def::EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: Some("authored mask graph".to_string()),
            description: Some("nondefault test graph".to_string()),
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: Vec::new(),
            wires: Vec::new(),
        });
        mask
    }

    fn master_target() -> EffectTarget {
        EffectTarget::Master
    }

    #[test]
    fn blob_v2_mask_save_undo_redo() {
        let mut project = Project::default();
        project.settings.master_effects.push(effect("Colour"));

        let mut mask = effect("MaskBlob");
        mask.params = ParamManifest::from_params(
            [
                ("threshold", "Threshold", 0.0, 1.0, 0.5),
                ("min_area", "Min Area", 0.0, 0.25, 0.001),
                ("max_blobs", "Max Blobs", 1.0, 32.0, 8.0),
                ("selection", "Selection", 0.0, 1.0, 0.0),
                ("amount", "Amount", 0.0, 1.0, 1.0),
            ]
            .into_iter()
            .map(|(id, name, min, max, default_value)| {
                Param::bundled(ParamSpecDef {
                    id: id.to_string(),
                    name: name.to_string(),
                    min,
                    max,
                    default_value,
                    whole_numbers: id == "max_blobs" || id == "selection",
                    value_labels: if id == "selection" {
                        vec!["All".to_string(), "Largest".to_string()]
                    } else {
                        Vec::new()
                    },
                    ..ParamSpecDef::default()
                })
            })
            .collect(),
        );
        mask.base_tracked = true;
        assert!(mask.set_base_param("threshold", 0.63));
        assert!(mask.set_base_param("min_area", 0.012));
        assert!(mask.set_base_param("max_blobs", 12.0));
        assert!(mask.set_base_param("selection", 1.0));
        assert!(mask.set_base_param("amount", 0.8));

        let mask_id = mask.id.clone();
        let mut command = AddGroupMaskCommand::new(master_target(), vec![0], mask);
        command.execute(&mut project);
        assert!(command.was_applied());

        let group = project.settings.master_effect_groups.as_ref().unwrap()[0].clone();
        let effects = &project.settings.master_effects;
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0].id, mask_id);
        assert_eq!(effects[0].effect_type(), &PresetTypeId::new("MaskBlob"));
        assert_eq!(effects[0].get_base_param("threshold"), 0.63);
        assert_eq!(effects[0].get_base_param("min_area"), 0.012);
        assert_eq!(effects[0].get_base_param("max_blobs"), 12.0);
        assert_eq!(effects[0].get_base_param("selection"), 1.0);
        assert_eq!(effects[0].get_base_param("amount"), 0.8);
        assert_eq!(group.mask_effect_id, Some(mask_id.clone()));
        assert_eq!(effects[0].group_id, Some(group.id.clone()));

        command.undo(&mut project);
        assert_eq!(project.settings.master_effects.len(), 1);
        assert!(
            project
                .settings
                .master_effect_groups
                .as_ref()
                .unwrap()
                .is_empty()
        );

        command.execute(&mut project);
        let redo_group = project.settings.master_effect_groups.as_ref().unwrap()[0].clone();
        assert_eq!(redo_group.id, group.id);
        assert_eq!(redo_group.mask_effect_id, Some(mask_id));

        let saved = serde_json::to_value(&project).unwrap();
        let saved_mask = saved["settings"]["masterEffects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|effect| effect["effectType"] == "MaskBlob")
            .unwrap();
        assert!(
            (saved_mask["params"]["threshold"]["value"].as_f64().unwrap() - 0.63).abs() < 1.0e-6
        );
        assert_eq!(saved_mask["params"]["max_blobs"]["value"], 12.0);
        assert_eq!(saved_mask["params"]["selection"]["value"], 1.0);

        let loaded: Project = serde_json::from_value(saved).unwrap();
        let loaded_mask = loaded
            .settings
            .master_effects
            .iter()
            .find(|effect| effect.effect_type() == &PresetTypeId::new("MaskBlob"))
            .unwrap();
        assert_eq!(loaded_mask.group_id, Some(redo_group.id));
        assert_eq!(loaded_mask.effect_type(), &PresetTypeId::new("MaskBlob"));
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
        assert!(
            project
                .settings
                .master_effect_groups
                .as_ref()
                .unwrap()
                .is_empty()
        );
        command.execute(&mut project);
        let redo_group = project.settings.master_effect_groups.as_ref().unwrap()[0].clone();
        assert_eq!(redo_group.id, group_id);
        assert_eq!(redo_group.mask_effect_id, Some(mask_id));
    }

    #[test]
    fn group_mask_by_id_follows_reordered_members_and_undo_redo() {
        let mut project = Project::default();
        project.settings.master_effects =
            vec![effect("First"), effect("Second"), effect("Outside")];
        let mut group =
            GroupEffectsCommand::new(master_target(), vec![0, 1], "Modifier Group".into());
        group.execute(&mut project);
        let group_id = project.settings.master_effect_groups.as_ref().unwrap()[0]
            .id
            .clone();
        let mask = effect("Mask");
        let mask_id = mask.id.clone();
        let mut command = AddGroupMaskCommand::for_group(master_target(), group_id.clone(), mask);
        // A queued menu command must resolve the group, not its old indices.
        project.settings.master_effects.rotate_right(1);
        let before = project
            .settings
            .master_effects
            .iter()
            .map(|effect| effect.id.clone())
            .collect::<Vec<_>>();
        command.execute(&mut project);
        assert!(command.was_applied());
        assert_eq!(project.settings.master_effects[0].id, before[0]);
        assert_eq!(project.settings.master_effects[1].id, mask_id);
        assert!(
            project.settings.master_effects[1..]
                .iter()
                .all(|effect| effect.group_id.as_ref() == Some(&group_id))
        );
        command.undo(&mut project);
        assert_eq!(
            project
                .settings
                .master_effects
                .iter()
                .map(|effect| effect.id.clone())
                .collect::<Vec<_>>(),
            before
        );
        command.execute(&mut project);
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            Some(mask_id)
        );
    }

    #[test]
    fn group_mask_by_id_rejects_deleted_group_without_wrapping_other_effects() {
        let mut project = Project::default();
        project.settings.master_effects = vec![effect("First")];
        let mut group = GroupEffectsCommand::new(master_target(), vec![0], "Modifier Group".into());
        group.execute(&mut project);
        let group_id = project.settings.master_effect_groups.as_ref().unwrap()[0]
            .id
            .clone();
        let mut command = AddGroupMaskCommand::for_group(master_target(), group_id, effect("Mask"));
        group.undo(&mut project);
        command.execute(&mut project);
        assert!(!command.was_applied());
        assert_eq!(project.settings.master_effects.len(), 1);
        assert!(project.settings.master_effects[0].group_id.is_none());
        assert!(
            project
                .settings
                .master_effect_groups
                .as_ref()
                .unwrap()
                .is_empty()
        );
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
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            Some(mask_id.clone())
        );

        command.undo(&mut project);
        assert_eq!(
            project
                .settings
                .master_effects
                .iter()
                .map(|effect| effect.id.clone())
                .collect::<Vec<_>>(),
            original_ids
        );
        assert_eq!(project.settings.master_effects[0].group_id, Some(gid));
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            None
        );
    }

    #[test]
    fn group_mask_remove_and_undo_redo_restores_reference() {
        let mut project = Project::default();
        project.settings.master_effects.push(effect("Colour"));
        let mask = effect("Mask");
        let mask_id = mask.id.clone();
        let mut add = AddGroupMaskCommand::new(master_target(), vec![0], mask);
        add.execute(&mut project);
        let group_id = project.settings.master_effect_groups.as_ref().unwrap()[0]
            .id
            .clone();
        let mask_index = project
            .settings
            .master_effects
            .iter()
            .position(|effect| effect.id == mask_id)
            .unwrap();
        let removed = project.settings.master_effects[mask_index].clone();
        let mut remove = RemoveEffectCommand::new(master_target(), removed, mask_index);

        remove.execute(&mut project);
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            None
        );
        remove.undo(&mut project);
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            Some(mask_id.clone())
        );
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].id,
            group_id
        );
        remove.execute(&mut project);
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            None
        );
    }

    #[test]
    fn group_mask_replace_preserves_group_members_and_restores_exact_mask() {
        let mut project = Project::default();
        project.settings.master_effects = vec![effect("Colour"), effect("Wet")];
        let mut group =
            GroupEffectsCommand::new(master_target(), vec![0, 1], "Modifier Group".into());
        group.execute(&mut project);
        let group_id = project.settings.master_effect_groups.as_ref().unwrap()[0]
            .id
            .clone();

        let first_mask = mask_with_state("MaskCircle", 0.25);
        let first_mask_id = first_mask.id.clone();
        let mut add = AddGroupMaskCommand::for_group(master_target(), group_id.clone(), first_mask);
        add.execute(&mut project);
        assert!(add.was_applied());
        let before_replace = project.settings.master_effects.clone();

        let replacement = mask_with_state("MaskGradient", 0.75);
        let replacement_id = replacement.id.clone();
        let mut replace =
            AddGroupMaskCommand::for_group(master_target(), group_id.clone(), replacement);
        replace.execute(&mut project);
        assert!(replace.was_applied());
        assert!(
            !project
                .settings
                .master_effects
                .iter()
                .any(|effect| effect.id == first_mask_id)
        );
        assert!(
            project
                .settings
                .master_effects
                .iter()
                .any(|effect| effect.id == replacement_id)
        );
        assert_eq!(project.settings.master_effects.len(), before_replace.len());
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].id,
            group_id
        );
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            Some(replacement_id.clone())
        );

        replace.undo(&mut project);
        assert_eq!(
            serde_json::to_value(&project.settings.master_effects).unwrap(),
            serde_json::to_value(&before_replace).unwrap()
        );
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            Some(first_mask_id)
        );
        replace.execute(&mut project);
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            Some(replacement_id)
        );
    }

    #[test]
    fn group_mask_remove_preserves_group_and_undoes_exact_mask() {
        let mut project = Project::default();
        project.settings.master_effects = vec![effect("Colour"), effect("Wet")];
        let mut group =
            GroupEffectsCommand::new(master_target(), vec![0, 1], "Modifier Group".into());
        group.execute(&mut project);
        let group_id = project.settings.master_effect_groups.as_ref().unwrap()[0]
            .id
            .clone();
        let mask = mask_with_state("MaskBlob", 0.63);
        let mask_id = mask.id.clone();
        let mut add = AddGroupMaskCommand::for_group(master_target(), group_id.clone(), mask);
        add.execute(&mut project);
        let before_remove = project.settings.master_effects.clone();

        let mut remove = RemoveGroupMaskCommand::new(master_target(), group_id.clone());
        remove.execute(&mut project);
        assert!(remove.was_applied());
        assert_eq!(
            project.settings.master_effects.len(),
            before_remove.len() - 1
        );
        assert!(
            !project
                .settings
                .master_effects
                .iter()
                .any(|effect| effect.id == mask_id)
        );
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].id,
            group_id
        );
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            None
        );

        remove.undo(&mut project);
        assert_eq!(
            serde_json::to_value(&project.settings.master_effects).unwrap(),
            serde_json::to_value(&before_remove).unwrap()
        );
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            Some(mask_id.clone())
        );
        remove.execute(&mut project);
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            None
        );
    }

    #[test]
    fn group_mask_remove_rejects_stale_cross_group_reference() {
        let mut project = Project::default();
        let other_group = EffectGroup::new("Other".into());
        let other_group_id = other_group.id.clone();
        let mut mask = mask_with_state("MaskBlob", 0.5);
        mask.group_id = Some(other_group_id.clone());
        let mask_id = mask.id.clone();
        let mut target_group = EffectGroup::new("Target".into());
        let target_group_id = target_group.id.clone();
        target_group.mask_effect_id = Some(mask_id.clone());
        project.settings.master_effects.push(mask);
        project.settings.master_effect_groups = Some(vec![target_group, other_group]);
        let before = serde_json::to_value(&project.settings.master_effects).unwrap();

        let mut remove = RemoveGroupMaskCommand::new(master_target(), target_group_id.clone());
        remove.execute(&mut project);
        assert!(!remove.was_applied());
        assert_eq!(
            remove.rejection_reason(),
            Some("group mask effect belongs to another group")
        );
        assert_eq!(
            serde_json::to_value(&project.settings.master_effects).unwrap(),
            before
        );
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            Some(mask_id)
        );
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
        let original_ids: Vec<EffectId> = project
            .settings
            .master_effects
            .iter()
            .map(|effect| effect.id.clone())
            .collect();
        let mut ungroup = UngroupEffectsCommand::new(master_target(), gid.clone());

        ungroup.execute(&mut project);
        assert!(
            project
                .settings
                .master_effects
                .iter()
                .all(|effect| effect.id != mask_id)
        );
        assert!(
            project
                .settings
                .master_effect_groups
                .as_ref()
                .unwrap()
                .is_empty()
        );
        ungroup.undo(&mut project);
        assert_eq!(
            project
                .settings
                .master_effects
                .iter()
                .map(|effect| effect.id.clone())
                .collect::<Vec<_>>(),
            original_ids
        );
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].id,
            gid
        );
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            Some(mask_id.clone())
        );
        ungroup.execute(&mut project);
        assert!(
            project
                .settings
                .master_effects
                .iter()
                .all(|effect| effect.id != mask_id)
        );
    }

    #[test]
    fn group_mask_structural_commands_reject_split_membership() {
        let mut project = Project::default();
        project.settings.master_effects.push(effect("Colour"));
        let mask = effect("Mask");
        let mut add = AddGroupMaskCommand::new(master_target(), vec![0], mask);
        add.execute(&mut project);
        let gid = project.settings.master_effect_groups.as_ref().unwrap()[0]
            .id
            .clone();

        let mut insert = AddEffectCommand::new(master_target(), effect("Inserted"), 1);
        insert.execute(&mut project);
        assert!(insert.rejection_reason().is_some());

        project.settings.master_effects.push(effect("Outside"));
        let mut move_into = ReorderEffectCommand::new(master_target(), 2, 1);
        move_into.execute(&mut project);
        assert!(move_into.rejection_reason().is_some());
        assert!(
            project
                .settings
                .master_effects
                .iter()
                .any(|effect| effect.group_id.as_ref() == Some(&gid))
        );

        let old_effects = project.settings.master_effects.clone();
        let mut broken = old_effects.clone();
        let outside = broken.pop().unwrap();
        broken.insert(1, outside);
        let mut snapshot = ReorderEffectGroupCommand::new(master_target(), old_effects, broken);
        snapshot.execute(&mut project);
        assert!(snapshot.rejection_reason().is_some());
    }

    #[test]
    fn set_group_collapsed_roundtrips_exactly() {
        let mut project = Project::default();
        project.settings.master_effects.push(effect("Colour"));
        let mut group = EffectGroup::new("Collapsed".into());
        group.collapsed = false;
        let group_id = group.id.clone();
        project.settings.master_effects[0].group_id = Some(group_id.clone());
        project.settings.master_effect_groups = Some(vec![group]);

        let mut command = SetGroupCollapsedCommand::new(master_target(), group_id, true);
        command.execute(&mut project);
        assert!(command.was_applied());
        assert!(project.settings.master_effect_groups.as_ref().unwrap()[0].collapsed);
        command.undo(&mut project);
        assert!(!project.settings.master_effect_groups.as_ref().unwrap()[0].collapsed);
        command.execute(&mut project);
        assert!(project.settings.master_effect_groups.as_ref().unwrap()[0].collapsed);

        let mut no_op = SetGroupCollapsedCommand::new(
            master_target(),
            project.settings.master_effect_groups.as_ref().unwrap()[0]
                .id
                .clone(),
            true,
        );
        no_op.execute(&mut project);
        assert!(!no_op.was_applied());
        assert_eq!(no_op.rejection_reason(), None);
    }

    #[test]
    fn masked_card_reorder_stays_in_group_but_partial_move_out_rejects() {
        let mut project = Project::default();
        let mut mask = effect("Mask");
        let mut first = effect("First");
        let mut second = effect("Second");
        let outside = effect("Outside");
        let mut group = EffectGroup::new("Masked".into());
        let group_id = group.id.clone();
        group.mask_effect_id = Some(mask.id.clone());
        mask.group_id = Some(group_id.clone());
        first.group_id = Some(group_id.clone());
        second.group_id = Some(group_id.clone());
        let second_id = second.id.clone();
        let first_id = first.id.clone();
        project.settings.master_effects = vec![mask, first, second, outside];
        project.settings.master_effect_groups = Some(vec![group]);
        let original_ids: Vec<EffectId> = project
            .settings
            .master_effects
            .iter()
            .map(|effect| effect.id.clone())
            .collect();

        let mut reorder = MoveEffectsToGroupCommand::new(
            master_target(),
            vec![second_id.clone()],
            Some(first_id.clone()),
            Some(group_id.clone()),
            false,
        );
        reorder.execute(&mut project);
        assert!(reorder.was_applied());
        assert_eq!(project.settings.master_effects[1].id, second_id);
        assert_eq!(
            project.settings.master_effect_groups.as_ref().unwrap()[0].mask_effect_id,
            project.settings.master_effects.first().map(|effect| effect.id.clone())
        );
        reorder.undo(&mut project);
        assert_eq!(
            project
                .settings
                .master_effects
                .iter()
                .map(|effect| effect.id.clone())
                .collect::<Vec<_>>(),
            original_ids
        );

        let mut move_out = MoveEffectsToGroupCommand::new(
            master_target(),
            vec![first_id],
            Some(project.settings.master_effects[3].id.clone()),
            None,
            false,
        );
        move_out.execute(&mut project);
        assert!(!move_out.was_applied());
        assert_eq!(
            move_out.rejection_reason(),
            Some("masked groups must move as a whole")
        );
    }

    #[test]
    fn separated_complete_groups_move_together_and_undo_exactly() {
        let mut project = Project::default();
        let mut a1 = effect("A1");
        let mut a2 = effect("A2");
        let middle = effect("Middle");
        let mut b1 = effect("B1");
        let mut b2 = effect("B2");
        let tail = effect("Tail");
        let group_a = EffectGroup::new("A".into());
        let group_b = EffectGroup::new("B".into());
        let group_a_id = group_a.id.clone();
        let group_b_id = group_b.id.clone();
        a1.group_id = Some(group_a_id.clone());
        a2.group_id = Some(group_a_id.clone());
        b1.group_id = Some(group_b_id.clone());
        b2.group_id = Some(group_b_id.clone());
        let tail_id = tail.id.clone();
        project.settings.master_effects = vec![a1, a2, middle, b1, b2, tail];
        project.settings.master_effect_groups = Some(vec![group_a, group_b]);
        let original_ids: Vec<EffectId> = project
            .settings
            .master_effects
            .iter()
            .map(|effect| effect.id.clone())
            .collect();

        let selected = project.settings.master_effects[..2]
            .iter()
            .chain(project.settings.master_effects[3..5].iter())
            .map(|effect| effect.id.clone())
            .collect();
        let mut move_groups = MoveEffectsToGroupCommand::new(
            master_target(),
            selected,
            Some(tail_id),
            None,
            true,
        );
        move_groups.execute(&mut project);
        assert!(move_groups.was_applied());
        assert_eq!(project.settings.master_effects[0].effect_type(), &PresetTypeId::new("Middle"));
        assert_eq!(project.settings.master_effects[1].group_id, Some(group_a_id));
        assert_eq!(project.settings.master_effects[3].group_id, Some(group_b_id));
        move_groups.undo(&mut project);
        assert_eq!(
            project
                .settings
                .master_effects
                .iter()
                .map(|effect| effect.id.clone())
                .collect::<Vec<_>>(),
            original_ids
        );
    }
}
