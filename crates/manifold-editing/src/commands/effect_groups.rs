use crate::command::Command;
use crate::commands::effect_target::{EffectTarget, with_effects_mut};
use manifold_core::project::Project;
use manifold_core::effects::{EffectGroup, EffectInstance};

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
    old_group_ids: Vec<Option<String>>,
    /// Original indices before MakeContiguous — used for undo RestoreOriginalOrder.
    original_indices: Vec<usize>,
}

impl GroupEffectsCommand {
    pub fn new(target: EffectTarget, effect_indices: Vec<usize>, group_name: String) -> Self {
        Self {
            target,
            effect_indices,
            group: Some(EffectGroup::new(group_name)),
            old_group_ids: Vec::new(),
            original_indices: Vec::new(),
        }
    }
}

impl Command for GroupEffectsCommand {
    fn execute(&mut self, project: &mut Project) {
        let indices = self.effect_indices.clone();
        let group = self.group.clone().unwrap();
        let group_id = group.id.clone();

        with_effects_mut(project, &self.target, |effects, groups| {
            // Snapshot old group IDs for undo
            self.old_group_ids.clear();
            for &i in &indices {
                self.old_group_ids.push(
                    effects.get(i).and_then(|e| e.group_id.clone())
                );
            }

            // Snapshot original indices for undo
            self.original_indices = indices.clone();

            // Collect the grouped effects (in selection order, which preserves relative order)
            let grouped: Vec<EffectInstance> = indices.iter()
                .filter_map(|&i| effects.get(i).cloned())
                .collect();

            if grouped.len() <= 1 {
                // No reorder needed for 0-1 effects, just assign group ID
                for &idx in &indices {
                    if let Some(effect) = effects.get_mut(idx) {
                        effect.group_id = Some(group_id.clone());
                    }
                }
                groups.push(group);
                return;
            }

            // MakeContiguous: find the index of the first grouped effect
            let mut insert_at = effects.len();
            let grouped_at_indices: Vec<bool> = (0..effects.len())
                .map(|i| indices.contains(&i))
                .collect();
            for (i, &is_grouped) in grouped_at_indices.iter().enumerate() {
                if is_grouped {
                    insert_at = i;
                    break;
                }
            }

            // Remove all grouped effects from the list (reverse order to preserve indices)
            let mut sorted_indices: Vec<usize> = indices.clone();
            sorted_indices.sort_unstable();
            for &idx in sorted_indices.iter().rev() {
                if idx < effects.len() {
                    effects.remove(idx);
                }
            }

            // Clamp insertAt after removals
            if insert_at > effects.len() {
                insert_at = effects.len();
            }

            // Re-insert in order at the target position, assigning group ID
            for (i, mut fx) in grouped.into_iter().enumerate() {
                fx.group_id = Some(group_id.clone());
                effects.insert(insert_at + i, fx);
            }

            groups.push(group);
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let original_indices = self.original_indices.clone();
        let old_group_ids = self.old_group_ids.clone();
        let group_id = self.group.as_ref().map(|g| g.id.clone());

        with_effects_mut(project, &self.target, |effects, groups| {
            // Find all effects in this group (they are contiguous after execute)
            let grouped: Vec<EffectInstance> = if let Some(ref gid) = group_id {
                effects.iter()
                    .filter(|e| e.group_id.as_deref() == Some(gid.as_str()))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };

            // Remove grouped effects from list
            if let Some(ref gid) = group_id {
                effects.retain(|e| e.group_id.as_deref() != Some(gid.as_str()));
            }

            // Restore original group IDs and re-insert at original positions
            // Sort by original index (ascending) so insertions don't shift subsequent indices
            let mut pairs: Vec<(usize, EffectInstance)> = Vec::with_capacity(grouped.len());
            for (i, mut fx) in grouped.into_iter().enumerate() {
                // Restore old group ID
                fx.group_id = old_group_ids.get(i).cloned().flatten();
                let idx = original_indices.get(i).copied().unwrap_or(effects.len());
                pairs.push((idx, fx));
            }
            pairs.sort_by_key(|(idx, _)| *idx);

            for (idx, fx) in pairs {
                let insert_at = idx.min(effects.len());
                effects.insert(insert_at, fx);
            }

            // Remove the group
            if let Some(ref gid) = group_id {
                groups.retain(|g| g.id != *gid);
            }
        });
    }

    fn description(&self) -> &str { "Group Effects" }
}

/// Ungroup effects from a rack group.
#[derive(Debug)]
pub struct UngroupEffectsCommand {
    target: EffectTarget,
    group_id: String,
    group: Option<EffectGroup>,
    member_indices: Vec<usize>,
}

impl UngroupEffectsCommand {
    pub fn new(target: EffectTarget, group_id: String) -> Self {
        Self { target, group_id, group: None, member_indices: Vec::new() }
    }
}

impl Command for UngroupEffectsCommand {
    fn execute(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();

        with_effects_mut(project, &self.target, |effects, groups| {
            self.group = groups.iter().find(|g| g.id == gid).cloned();

            self.member_indices = effects.iter().enumerate()
                .filter(|(_, e)| e.group_id.as_deref() == Some(&gid))
                .map(|(i, _)| i)
                .collect();

            for effect in effects.iter_mut() {
                if effect.group_id.as_deref() == Some(&gid) {
                    effect.group_id = None;
                }
            }

            groups.retain(|g| g.id != gid);
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();
        let group = self.group.clone();
        let member_indices = self.member_indices.clone();

        with_effects_mut(project, &self.target, |effects, groups| {
            for &idx in &member_indices {
                if let Some(effect) = effects.get_mut(idx) {
                    effect.group_id = Some(gid.clone());
                }
            }

            if let Some(g) = group {
                groups.push(g);
            }
        });
    }

    fn description(&self) -> &str { "Ungroup Effects" }
}

/// Toggle a group's enabled state.
#[derive(Debug)]
pub struct ToggleGroupCommand {
    target: EffectTarget,
    group_id: String,
    old_enabled: bool,
    new_enabled: bool,
}

impl ToggleGroupCommand {
    pub fn new(target: EffectTarget, group_id: String, old_enabled: bool, new_enabled: bool) -> Self {
        Self { target, group_id, old_enabled, new_enabled }
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

    fn description(&self) -> &str { "Toggle Group" }
}

/// Rename a group.
#[derive(Debug)]
pub struct RenameGroupCommand {
    target: EffectTarget,
    group_id: String,
    old_name: String,
    new_name: String,
}

impl RenameGroupCommand {
    pub fn new(target: EffectTarget, group_id: String, old_name: String, new_name: String) -> Self {
        Self { target, group_id, old_name, new_name }
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

    fn description(&self) -> &str { "Rename Group" }
}

/// Change group wet/dry mix.
#[derive(Debug)]
pub struct ChangeGroupWetDryCommand {
    target: EffectTarget,
    group_id: String,
    old_wet_dry: f32,
    new_wet_dry: f32,
}

impl ChangeGroupWetDryCommand {
    pub fn new(target: EffectTarget, group_id: String, old_wet_dry: f32, new_wet_dry: f32) -> Self {
        Self { target, group_id, old_wet_dry, new_wet_dry }
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

    fn description(&self) -> &str { "Change Group Wet/Dry" }
}

/// Move an effect to a different rack (change group assignment + index).
/// Matches Unity MoveEffectToRackCommand.
#[derive(Debug)]
pub struct MoveEffectToRackCommand {
    target: EffectTarget,
    #[allow(dead_code)]
    effect_index: usize,
    old_group_id: Option<String>,
    new_group_id: Option<String>,
    old_index: usize,
    new_index: usize,
}

impl MoveEffectToRackCommand {
    pub fn new(
        target: EffectTarget,
        effect_index: usize,
        old_group_id: Option<String>,
        new_group_id: Option<String>,
        old_index: usize,
        new_index: usize,
    ) -> Self {
        Self { target, effect_index, old_group_id, new_group_id, old_index, new_index }
    }
}

impl Command for MoveEffectToRackCommand {
    fn execute(&mut self, project: &mut Project) {
        let from = self.old_index;
        let to = self.new_index;
        let new_gid = self.new_group_id.clone();
        with_effects_mut(project, &self.target, |effects, _groups| {
            if from < effects.len() {
                let mut effect = effects.remove(from);
                effect.group_id = new_gid;
                // After remove, if from < to the target shifted down by 1
                let insert_idx = if from < to { to - 1 } else { to };
                let insert_idx = insert_idx.min(effects.len());
                effects.insert(insert_idx, effect);
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let from = self.old_index;
        let to = self.new_index;
        let old_gid = self.old_group_id.clone();
        with_effects_mut(project, &self.target, |effects, _groups| {
            let adjusted_to = if from < to { to - 1 } else { to };
            let adjusted_to = adjusted_to.min(effects.len().saturating_sub(1));
            if adjusted_to < effects.len() {
                let mut effect = effects.remove(adjusted_to);
                effect.group_id = old_gid;
                let insert_idx = from.min(effects.len());
                effects.insert(insert_idx, effect);
            }
        });
    }

    fn description(&self) -> &str { "Move Effect to Rack" }
}

/// Move an entire rack (all effects with matching groupId) to a new position.
/// Maintains contiguity invariant. Matches Unity ReorderRackCommand.
#[derive(Debug)]
pub struct ReorderRackCommand {
    target: EffectTarget,
    group_id: String,
    target_insert_index: usize,
    /// Original indices of all group members, captured on first execute.
    original_indices: Vec<usize>,
}

impl ReorderRackCommand {
    pub fn new(target: EffectTarget, group_id: String, target_insert_index: usize) -> Self {
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
            let members: Vec<EffectInstance> = self.original_indices.iter()
                .filter_map(|&i| effects.get(i).cloned())
                .collect();

            // Count how many members were before the target (their removal shifts target down)
            let removed_before = self.original_indices.iter()
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
            let members: Vec<EffectInstance> = effects.iter()
                .filter(|e| e.group_id.as_deref() == Some(&gid))
                .cloned()
                .collect();

            // Remove all members
            effects.retain(|e| e.group_id.as_deref() != Some(&gid));

            // Re-insert at original positions (ascending order)
            let mut pairs: Vec<(usize, EffectInstance)> = original_indices.iter()
                .zip(members.into_iter())
                .map(|(&idx, fx)| (idx, fx))
                .collect();
            pairs.sort_by_key(|(idx, _)| *idx);

            for (idx, fx) in pairs {
                let insert_at = idx.min(effects.len());
                effects.insert(insert_at, fx);
            }
        });
    }

    fn description(&self) -> &str { "Move Rack" }
}
