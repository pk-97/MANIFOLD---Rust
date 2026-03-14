use crate::command::Command;
use crate::commands::effect_target::{EffectTarget, with_effects_mut};
use manifold_core::project::Project;
use manifold_core::effects::EffectGroup;

/// Group effects into a rack group.
#[derive(Debug)]
pub struct GroupEffectsCommand {
    target: EffectTarget,
    effect_indices: Vec<usize>,
    group: Option<EffectGroup>,
    old_group_ids: Vec<Option<String>>,
}

impl GroupEffectsCommand {
    pub fn new(target: EffectTarget, effect_indices: Vec<usize>, group_name: String) -> Self {
        Self {
            target,
            effect_indices,
            group: Some(EffectGroup::new(group_name)),
            old_group_ids: Vec::new(),
        }
    }
}

impl Command for GroupEffectsCommand {
    fn execute(&mut self, project: &mut Project) {
        let indices = self.effect_indices.clone();
        let group = self.group.clone().unwrap();
        let group_id = group.id.clone();

        with_effects_mut(project, &self.target, |effects, groups| {
            // Save old group IDs for undo
            self.old_group_ids = indices.iter()
                .map(|&i| effects.get(i).and_then(|e| e.group_id.clone()))
                .collect();

            // Assign group ID to selected effects
            for &idx in &indices {
                if let Some(effect) = effects.get_mut(idx) {
                    effect.group_id = Some(group_id.clone());
                }
            }

            // Add group to groups list
            groups.push(group);
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let indices = self.effect_indices.clone();
        let old_ids = self.old_group_ids.clone();
        let group_id = self.group.as_ref().map(|g| g.id.clone());

        with_effects_mut(project, &self.target, |effects, groups| {
            // Restore old group IDs
            for (i, &idx) in indices.iter().enumerate() {
                if let Some(effect) = effects.get_mut(idx) {
                    effect.group_id = old_ids.get(i).cloned().flatten();
                }
            }

            // Remove the group
            if let Some(gid) = &group_id {
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
            // Save group for undo
            self.group = groups.iter().find(|g| g.id == gid).cloned();

            // Find member indices
            self.member_indices = effects.iter().enumerate()
                .filter(|(_, e)| e.group_id.as_deref() == Some(&gid))
                .map(|(i, _)| i)
                .collect();

            // Clear group ID on members
            for effect in effects.iter_mut() {
                if effect.group_id.as_deref() == Some(&gid) {
                    effect.group_id = None;
                }
            }

            // Remove group
            groups.retain(|g| g.id != gid);
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let gid = self.group_id.clone();
        let group = self.group.clone();
        let member_indices = self.member_indices.clone();

        with_effects_mut(project, &self.target, |effects, groups| {
            // Restore group ID on members
            for &idx in &member_indices {
                if let Some(effect) = effects.get_mut(idx) {
                    effect.group_id = Some(gid.clone());
                }
            }

            // Restore group
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
                let insert_idx = to.min(effects.len());
                effects.insert(insert_idx, effect);
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let to = self.new_index;
        let from = self.old_index;
        let old_gid = self.old_group_id.clone();
        with_effects_mut(project, &self.target, |effects, _groups| {
            let actual_to = to.min(effects.len().saturating_sub(1));
            if actual_to < effects.len() {
                let mut effect = effects.remove(actual_to);
                effect.group_id = old_gid;
                let insert_idx = from.min(effects.len());
                effects.insert(insert_idx, effect);
            }
        });
    }

    fn description(&self) -> &str { "Move Effect to Rack" }
}
